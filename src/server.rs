use std::{
    fs::{self, File},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    http::server::{Configuration, EspHttpConnection, EspHttpServer, Request},
    wifi::BlockingWifi,
};
use serde_json::json;
use log::{info, error};

use crate::{
    config::{AppConfig, MAX_UPLOAD_SIZE, SERVER_PORT, MAX_CONNECTIONS, DEFAULT_ROOT_FOLDER},
    storage,
    metadata::{self, ManifestManager, FileMetadata},
    system_info,
    wifi,
};
use std::sync::mpsc::{sync_channel, SyncSender, Receiver};


// We need to make sure our struct is Send + Sync
pub struct AppResources<'a> {
    pub wifi: Arc<Mutex<BlockingWifi<esp_idf_svc::wifi::EspWifi<'a>>>>,
    pub server: EspHttpServer<'a>,
    pub mounted_fatfs: esp_idf_svc::io::vfs::MountedFatfs<
        esp_idf_svc::fs::fatfs::Fatfs<
            esp_idf_svc::hal::sd::SdCardDriver<esp_idf_svc::hal::sd::mmc::SdMmcHostDriver<'a>>>,
        >,
    pub sys_loop: EspSystemEventLoop,
    pub config: AppConfig,
    pub manifest_manager: Arc<ManifestManager>, // Add ManifestManager
}

// Allow resources to be sent across threads
unsafe impl<'a> Send for AppResources<'a> {}
unsafe impl<'a> Sync for AppResources<'a> {}
const STACK_SIZE: usize = 1024 * 138; // Keep stack size reasonable

pub fn setup_http_server() -> anyhow::Result<EspHttpServer<'static>> {
    let mut server_config = Configuration {
        stack_size: STACK_SIZE,
        max_open_sockets: MAX_CONNECTIONS, // Limit connections
        ..Default::default()
    };
    server_config.uri_match_wildcard = true;
    server_config.http_port = SERVER_PORT;

    Ok(EspHttpServer::new(&server_config)?)
}

pub fn register_handlers(resources: Arc<Mutex<AppResources<'static>>>) -> anyhow::Result<()> {
    let locked_resources = resources.lock().map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let config = locked_resources.config.clone();
    let root_path = Arc::new(config.root_path.clone()); // Clone for handlers
    let chunk_size = config.chunk_size;
    let manifest_manager = locked_resources.manifest_manager.clone(); // Clone for handlers
    let wifi_handle = locked_resources.wifi.clone(); // Clone wifi for system handler

    drop(locked_resources); // Release the lock

    let server = &mut resources.lock().map_err(|e| anyhow::anyhow!("{e:?}"))?.server;


    // Handler for Metadata API
    let metadata_root_path = root_path.clone();
    let metadata_manifest_manager = manifest_manager.clone();
    server.fn_handler("/api/metadata/*", esp_idf_svc::http::Method::Get, move |req| {
        handle_metadata_request(req, metadata_root_path.clone(), metadata_manifest_manager.clone())
    })?;

    // Handler for System API
    let system_wifi_handle = wifi_handle.clone();
    let system_mount_point = Arc::new(PathBuf::from(crate::config::SD_CARD_MOUNT_POINT));
    server.fn_handler("/api/system", esp_idf_svc::http::Method::Get, move |req| {
        handle_system_request(req, system_wifi_handle.clone(), system_mount_point.clone())
    })?;

    // Handler for GET requests - Directory listing and file download
    let get_root_path = root_path.clone();
    let get_manifest_manager = manifest_manager.clone();
    server.fn_handler("/*", esp_idf_svc::http::Method::Get, move |req| {
        handle_get_request(req, get_root_path.clone(), chunk_size, get_manifest_manager.clone())
    })?;

    // Handler for POST requests - File upload
    let post_root_path = root_path.clone();
    let post_manifest_manager = manifest_manager.clone();
    server.fn_handler("/*", esp_idf_svc::http::Method::Post, move |req| {
        handle_post_request(req, post_root_path.clone(), post_manifest_manager.clone())
    })?;

    // Handler for DELETE requests - File/Folder deletion
    let delete_root_path = root_path.clone();
    let delete_manifest_manager = manifest_manager.clone();
    server.fn_handler("/*", esp_idf_svc::http::Method::Delete, move |req| {
        handle_delete_request(req, delete_root_path.clone(), delete_manifest_manager.clone())
    })?;

    // Handler for MKCOL (Create Directory) requests
    let mkdir_root_path = root_path.clone();
    let mkdir_manifest_manager = manifest_manager.clone();
    server.fn_handler("/*", esp_idf_svc::http::Method::MkCol, move |req| {
        handle_mkdir_request(req, mkdir_root_path.clone(), mkdir_manifest_manager.clone())
    })?;


    Ok(())
}

fn handle_get_request(
    req: Request<&mut EspHttpConnection>,
    root_path: Arc<PathBuf>,
    chunk_size: usize,
    manifest_manager: Arc<ManifestManager>,
) -> Result<(), anyhow::Error> {
    let path = req.uri();
    let full_path = root_path.join(path.trim_start_matches('/'));

    info!("Handling GET request for: {:?}", full_path);

    if !full_path.exists() {
        error!("Path not found: {:?}", full_path);
        if let Ok(mut resp) = req.into_response(404, Some("Not Found"), &[]) {
            resp.write(b"File or directory not found")?;
            resp.flush()?;
            resp.release();
        }
        return Ok(());
    }

    if full_path.is_dir() {
        // Directory listing - use manifest for faster listing
        info!("Listing directory: {:?}", full_path);
        match manifest_manager.get_or_load_manifest(&full_path) {
            Ok(manifest_arc) => {
                let manifest = manifest_arc.lock().map_err(|e| anyhow::anyhow!("Failed to lock manifest: {:?}", e))?;
                let mut content = String::from("<!DOCTYPE html><html><head><title>Directory Listing</title></head><body><h1>Directory Listing</h1><ul>");

                // Add parent directory link if not at root
                if path != "/" {
                    let path_buf = PathBuf::from(path);
                    let parent_path = path_buf.parent().unwrap_or_else(|| std::path::Path::new("/"));
                    content.push_str(&format!(
                        "<li><a href=\"{}\">.. (Parent Directory)</a></li>",
                        parent_path.display()
                    ));
                }

                let mut dirs = Vec::new();
                let mut files = Vec::new();

                for entry in manifest.entries.values() {
                    let entry_path = format!("{}/{}", path.trim_end_matches('/'), entry.name);
                    if entry.is_dir {
                        dirs.push(format!("<li><a href=\"{}\">{}/</a></li>", entry_path, entry.name));
                    } else {
                        files.push(format!("<li><a href=\"{}\">{}</a></li>", entry_path, entry.name));
                    }
                }

                // Sort and append directories first, then files
                dirs.sort();
                files.sort();
                content.push_str(&dirs.join(""));
                content.push_str(&files.join(""));

                content.push_str("</ul><hr><p>ESP32 Cloud Server</p></body></html>");

                if let Ok(mut resp) = req.into_response(200, Some("OK"), &[("Content-Type", "text/html")]) {
                    resp.write(content.as_bytes())?;
                    resp.flush()?;
                    resp.release();
                }
            }
            Err(e) => {
                error!("Error loading manifest for {:?}: {}", full_path, e);
                // Fallback to basic file system listing if manifest loading fails
                 let mut content = String::from("<!DOCTYPE html><html><head><title>Directory Listing (Fallback)</title></head><body><h1>Directory Listing (Fallback)</h1><ul>");
                 if path != "/" {
                    let path_buf = PathBuf::from(path);
                    let parent_path = path_buf.parent().unwrap_or_else(|| std::path::Path::new("/"));
                    content.push_str(&format!(
                        "<li><a href=\"{}\">.. (Parent Directory)</a></li>",
                        parent_path.display()
                    ));
                }
                if let Ok(entries) = fs::read_dir(&full_path) {
                    let mut dirs = Vec::new();
                    let mut files = Vec::new();

                    for entry in entries.flatten() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        // Skip manifest files in listing
                        if name == ".manifest.bin" || name == ".manifest.bin.tmp" {
                            continue;
                        }
                        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                        let entry_path = format!("{}/{}", path.trim_end_matches('/'), name);

                        if is_dir {
                            dirs.push(format!("<li><a href=\"{}\">{}/</a></li>", entry_path, name));
                        } else {
                            files.push(format!("<li><a href=\"{}\">{}</a></li>", entry_path, name));
                        }
                    }

                    dirs.sort();
                    files.sort();
                    content.push_str(&dirs.join(""));
                    content.push_str(&files.join(""));
                }
                content.push_str("</ul><hr><p>ESP32 Cloud Server</p></body></html>");
                 if let Ok(mut resp) = req.into_response(500, Some("Internal Server Error"), &[("Content-Type", "text/html")]) {
                    resp.write(content.as_bytes())?;
                    resp.flush()?;
                    resp.release();
                }
            }
        }
    } else {
        // File content with chunked downloading
        info!("Serving file: {:?}", full_path);
        match File::open(&full_path) {
            Ok(mut file) => {
                let content_type = storage::get_content_type(&full_path);
                let file_size = file.metadata().map(|m| m.len()).unwrap_or(0);

                // Get range header if present
                let range_header = req.header("Range").unwrap_or("").to_string();

                if range_header.starts_with("bytes=") {
                    // Handle range request
                    handle_range_request(
                        req,
                        &mut file,
                        file_size,
                        content_type,
                        &range_header,
                        chunk_size,
                    )?;
                } else {
                    // Handle full file request with chunking
                    if let Ok(mut resp) = req.into_response(
                        200,
                        Some("OK"),
                        &[
                            ("Content-Length", &format!("{file_size}")),
                            ("Content-Type", content_type),
                            ("Accept-Ranges", "bytes"),
                        ],
                    ) {
                        let mut buffer = vec![0; chunk_size];
                        loop {
                            match file.read(&mut buffer) {
                                Ok(0) => break, // End of file
                                Ok(bytes_read) => {
                                    resp.write(&buffer[..bytes_read])?;
                                    resp.flush()?;
                                }
                                Err(e) => {
                                    error!("Error reading file: {}", e);
                                    break;
                                }
                            }
                        }
                        resp.release();
                    }
                }
            }
            Err(e) => {
                error!("Error opening file {:?}: {}", full_path, e);
                if let Ok(mut resp) = req.into_status_response(404) {
                    resp.write(b"File not found or cannot be accessed")?;
                    resp.flush()?;
                    resp.release();
                }
            }
        }
    }

    Ok(())
}

fn handle_range_request(
    req: Request<&mut EspHttpConnection>,
    file: &mut File,
    file_size: u64,
    content_type: &str,
    range_header: &str,
    chunk_size: usize,
) -> Result<(), anyhow::Error> {
    info!("Handling Range request: {}", range_header);
    // Parse range header (format: "bytes=start-end")
    let range_part = range_header.trim_start_matches("bytes=");
    let (start, end) = if let Some((start_str, end_str)) = range_part.split_once('-') {
        let start = start_str.parse::<u64>().unwrap_or(0);
        let end = if end_str.is_empty() {
            file_size.saturating_sub(1) // Use saturating_sub to avoid underflow
        } else {
            end_str.parse::<u64>().unwrap_or(file_size.saturating_sub(1))
        };
        (start, std::cmp::min(end, file_size.saturating_sub(1)))
    } else {
        (0, file_size.saturating_sub(1))
    };

    if start >= file_size {
        // Invalid range
        if let Ok(mut resp) = req.into_response(416, Some("Range Not Satisfiable"), &[("Content-Range", &format!("bytes */{}", file_size))]) {
            resp.flush()?;
            resp.release();
        }
        return Ok(());
    }

    let content_length = end.saturating_sub(start) + 1; // Use saturating_sub
    let content_range = format!("bytes {}-{}/{}", start, end, file_size);

    // Seek to the requested position
    if let Err(e) = file.seek(std::io::SeekFrom::Start(start)) {
        error!("Error seeking file: {}", e);
        if let Ok(mut resp) = req.into_status_response(500) {
            resp.write(b"Error processing range request")?;
            resp.flush()?;
            resp.release();
        }
        return Ok(());
    }

    // Send partial content
    if let Ok(mut resp) = req.into_response(
        206,
        Some("Partial Content"),
        &[
            ("Content-Length", &format!("{content_length}")),
            ("Content-Type", content_type),
            ("Content-Range", &content_range),
            ("Accept-Ranges", "bytes"),
        ],
    ) {
        let mut bytes_remaining = content_length;
        let mut buffer = vec![0; std::cmp::min(chunk_size, bytes_remaining as usize)];

        while bytes_remaining > 0 {
            let to_read = std::cmp::min(buffer.len(), bytes_remaining as usize);
            match file.read(&mut buffer[..to_read]) {
                Ok(0) => break, // Unexpected EOF
                Ok(bytes_read) => {
                    resp.write(&buffer[..bytes_read])?;
                    resp.flush()?;
                    bytes_remaining -= bytes_read as u64;
                    if buffer.len() > bytes_remaining as usize {
                         // Resize buffer if remaining bytes are less than current buffer size
                        buffer.resize(bytes_remaining as usize, 0);
                    }
                }
                Err(e) => {
                    error!("Error reading file: {}", e);
                    break;
                }
            }
        }
        resp.release();
    }

    Ok(())
}


fn handle_post_request(
    mut req: Request<&mut EspHttpConnection>,
    root_path: Arc<PathBuf>,
    manifest_manager: Arc<ManifestManager>,
) -> Result<(), anyhow::Error> {
    info!("Received POST request for upload");
    let path = req.uri().to_string();
    let full_path = root_path.join(path.trim_start_matches('/'));

    info!("Upload path: {:?}", full_path);

    // Create parent directories if they don't exist
    if let Some(parent) = full_path.parent() {
        if !parent.exists() {
            if let Err(e) = storage::create_directory(parent) {
                error!("Failed to create directory structure: {}", e);
                if let Ok(mut resp) = req.into_status_response(500) {
                    resp.write(b"Failed to create directory structure")?;
                    resp.flush()?;
                    resp.release();
                }
                return Ok(());
            }
        }
    }

    // Check if we're trying to upload to a directory path
    if full_path.exists() && full_path.is_dir() {
        error!("Cannot upload to a directory path: {:?}", full_path);
        if let Ok(mut resp) = req.into_status_response(400) {
            resp.write(b"Cannot upload to a directory path")?;
            resp.flush()?;
            resp.release();
        }
        return Ok(());
    }

    // Get content length if available
    let content_length_str = req.header("Content-Length").unwrap_or("0");
    let content_length = content_length_str.parse::<usize>().unwrap_or(0);

    // Check if the upload is too large
    if content_length as u64 > MAX_UPLOAD_SIZE {
        error!("File too large: {} bytes (max {})", content_length, MAX_UPLOAD_SIZE);
        if let Ok(mut resp) = req.into_status_response(413) {
            resp.write(b"File too large")?;
            resp.flush()?;
            resp.release();
        }
        return Ok(());
    }

    info!("Creating file for upload: {:?}", full_path);

    // Create or open file for writing
    match File::create(&full_path) {
        Ok(mut file) => {
            let mut buffer = vec![0; 1024 * 128]; // 128KB buffer for reading chunks
            let mut total_bytes = 0;

            // Use a channel and a separate thread for writing to avoid blocking the HTTP server thread
            let full_path_clone = full_path.clone();
            let (tx, rx): (SyncSender<Vec<u8>>, Receiver<Vec<u8>>) = sync_channel(4); // 4 buffers in flight
            std::thread::spawn(move || {
                if let Ok(mut file) = File::create(full_path_clone) {
                    while let Ok(buffer) = rx.recv() {
                        if let Err(e) = file.write_all(&buffer) {
                            error!("Error writing to file: {}", e);
                            break;
                        }
                    }
                    if let Err(e) = file.flush() {
                        error!("Error flushing file: {}", e);
                    }
                } else {
                    error!("Failed to open file for writing in spawned thread");
                }
            });

            // Read request body in chunks
            loop {
                match req.read(&mut buffer) {
                    Ok(0) => break, // End of request
                    Ok(bytes_read) => {
                        if tx.send(buffer[..bytes_read].to_vec()).is_err() {
                            error!("Writer thread died or channel closed");
                            break;
                        }
                        total_bytes += bytes_read;
                    }
                    Err(e) => {
                        error!("Error reading request body: {}", e);
                        break;
                    }
                }
            }
            drop(tx); // Close the channel to signal the writer thread to finish

            // Wait for the writer thread to finish (optional, but ensures data is written before responding)
            // In a real application, you might want a more robust way to handle this.
            // For simplicity here, we rely on the channel being dropped.

            info!(
                "File upload received: {} ({} bytes)",
                full_path.display(),
                total_bytes
            );

            // Update manifest after successful upload
            if let Err(e) = manifest_manager.add_or_update_entry(&full_path) {
                error!("Failed to update manifest after upload: {}", e);
                // Respond with an error, but the file might still be on disk
                 if let Ok(mut resp) = req.into_status_response(500) {
                    resp.write(b"File uploaded, but failed to update metadata")?;
                    resp.flush()?;
                    resp.release();
                }
                return Ok(());
            }


            info!("File uploaded successfully and manifest updated: {:?}", full_path);
            if let Ok(mut resp) = req.into_response(201, Some("Created"), &[("Location", &path)]) {
                resp.write(
                    format!("File uploaded successfully ({} bytes)", total_bytes).as_bytes(),
                )?;
                resp.flush()?;
                resp.release();
            }
        }
        Err(e) => {
            error!("Failed to create file for upload: {}", e);
            if let Ok(mut resp) = req.into_status_response(500) {
                resp.write(b"Failed to create file on storage")?;
                resp.flush()?;
                resp.release();
            }
        }
    }

    Ok(())
}

fn handle_delete_request(
    req: Request<&mut EspHttpConnection>,
    root_path: Arc<PathBuf>,
    manifest_manager: Arc<ManifestManager>,
) -> Result<(), anyhow::Error> {
    let path = req.uri();
    let full_path = root_path.join(path.trim_start_matches('/'));

    info!("Handling DELETE request for: {:?}", full_path);

    if !full_path.exists() {
        error!("Path not found for deletion: {:?}", full_path);
        if let Ok(mut resp) = req.into_status_response(404) {
            resp.write(b"File or directory not found")?;
            resp.flush()?;
            resp.release();
        }
        return Ok(());
    }

    // Prevent deleting the root directory itself
    if full_path == *root_path {
        error!("Attempted to delete root directory: {:?}", full_path);
         if let Ok(mut resp) = req.into_status_response(400) {
            resp.write(b"Cannot delete root directory")?;
            resp.flush()?;
            resp.release();
        }
        return Ok(());
    }


    // Delete the file/directory
    if let Err(e) = storage::delete_entry(&full_path) {
        error!("Failed to delete {:?}: {}", full_path, e);
        if let Ok(mut resp) = req.into_status_response(500) {
            resp.write(b"Failed to delete file or directory")?;
            resp.flush()?;
            resp.release();
        }
        return Ok(());
    }

    // Update manifest after successful deletion
    if let Err(e) = manifest_manager.remove_entry(&full_path) {
        error!("Failed to update manifest after deletion: {}", e);
        // Respond with an error, but the file might be deleted from disk
         if let Ok(mut resp) = req.into_status_response(500) {
            resp.write(b"Entry deleted, but failed to update metadata")?;
            resp.flush()?;
            resp.release();
        }
        return Ok(());
    }


    info!("Successfully deleted {:?} and updated manifest", full_path);
    if let Ok(mut resp) = req.into_status_response(200) {
        resp.write(b"Successfully deleted")?;
        resp.flush()?;
        resp.release();
    }

    Ok(())
}

fn handle_mkdir_request(
    req: Request<&mut EspHttpConnection>,
    root_path: Arc<PathBuf>,
    manifest_manager: Arc<ManifestManager>,
) -> Result<(), anyhow::Error> {
    let path = req.uri();
    let full_path = root_path.join(path.trim_start_matches('/'));

    info!("Handling MKCOL request for: {:?}", full_path);

    if full_path.exists() {
        error!("Path already exists for MKCOL: {:?}", full_path);
        if let Ok(mut resp) = req.into_status_response(409) { // Conflict
            resp.write(b"Directory already exists")?;
            resp.flush()?;
            resp.release();
        }
        return Ok(());
    }

    // Create the directory
    if let Err(e) = storage::create_directory(&full_path) {
        error!("Failed to create directory {:?}: {}", full_path, e);
        if let Ok(mut resp) = req.into_status_response(500) {
            resp.write(b"Failed to create directory")?;
            resp.flush()?;
            resp.release();
        }
        return Ok(());
    }

    // Add entry to the parent directory's manifest
     if let Err(e) = manifest_manager.add_or_update_entry(&full_path) {
        error!("Failed to update manifest after directory creation: {}", e);
        // Respond with an error, but the directory might be created on disk
         if let Ok(mut resp) = req.into_status_response(500) {
            resp.write(b"Directory created, but failed to update metadata")?;
            resp.flush()?;
            resp.release();
        }
        return Ok(());
    }


    info!("Successfully created directory {:?} and updated manifest", full_path);
    if let Ok(mut resp) = req.into_status_response(201) { // Created
        resp.write(b"Directory created successfully")?;
        resp.flush()?;
        resp.release();
    }

    Ok(())
}

fn handle_metadata_request(
    req: Request<&mut EspHttpConnection>,
    root_path: Arc<PathBuf>,
    manifest_manager: Arc<ManifestManager>,
) -> Result<(), anyhow::Error> {
    let path = req.uri().trim_start_matches("/api/metadata");
    let full_path = root_path.join(path.trim_start_matches('/'));

    info!("Handling Metadata request for: {:?}", full_path);

    if !full_path.exists() {
        error!("Path not found for metadata: {:?}", full_path);
        if let Ok(mut resp) = req.into_status_response(404) {
            resp.write(b"File or directory not found")?;
            resp.flush()?;
            resp.release();
        }
        return Ok(());
    }

    if full_path.is_dir() {
        // Return metadata for all entries in the directory from the manifest
        match manifest_manager.get_or_load_manifest(&full_path) {
            Ok(manifest_arc) => {
                let manifest = manifest_arc.lock().map_err(|e| anyhow::anyhow!("Failed to lock manifest: {:?}", e))?;
                let metadata_list: Vec<&FileMetadata> = manifest.entries.values().collect();
                let json_response = json!(metadata_list).to_string();

                if let Ok(mut resp) = req.into_response(200, Some("OK"), &[("Content-Type", "application/json")]) {
                    resp.write(json_response.as_bytes())?;
                    resp.flush()?;
                    resp.release();
                }
            }
            Err(e) => {
                error!("Error loading manifest for metadata request {:?}: {}", full_path, e);
                if let Ok(mut resp) = req.into_status_response(500) {
                    resp.write(b"Failed to retrieve directory metadata")?;
                    resp.flush()?;
                    resp.release();
                }
            }
        }
    } else {
        // Return metadata for a single file
        let parent_folder = full_path.parent().ok_or_else(|| anyhow::anyhow!("Invalid file path for metadata: {:?}", full_path))?;
        let file_name = full_path.file_name().ok_or_else(|| anyhow::anyhow!("Invalid file name for metadata: {:?}", full_path))?.to_string_lossy().to_string();

        match manifest_manager.get_or_load_manifest(parent_folder) {
            Ok(manifest_arc) => {
                let manifest = manifest_arc.lock().map_err(|e| anyhow::anyhow!("Failed to lock manifest: {:?}", e))?;
                if let Some(metadata) = manifest.get_entry(&file_name) {
                    let json_response = json!(metadata).to_string();
                     if let Ok(mut resp) = req.into_response(200, Some("OK"), &[("Content-Type", "application/json")]) {
                        resp.write(json_response.as_bytes())?;
                        resp.flush()?;
                        resp.release();
                    }
                } else {
                    // Entry not found in manifest, try to generate it on the fly
                    error!("Metadata not found in manifest for {:?}, attempting to generate.", full_path);
                    match (storage::get_mtime(&full_path), metadata::calculate_crc32(&full_path), fs::metadata(&full_path)) {
                        (Ok(mtime), Ok(crc32), Ok(metadata)) => {
                            let file_metadata = FileMetadata {
                                name: file_name.clone(),
                                size: metadata.len(),
                                mtime,
                                crc32,
                                is_dir: false,
                            };
                             // Optionally add to manifest for future requests
                            let _ = manifest_manager.add_or_update_entry(&full_path); // Ignore error here

                            let json_response = json!(file_metadata).to_string();
                             if let Ok(mut resp) = req.into_response(200, Some("OK"), &[("Content-Type", "application/json")]) {
                                resp.write(json_response.as_bytes())?;
                                resp.flush()?;
                                resp.release();
                            }
                        }
                        _ => {
                             error!("Failed to generate metadata for {:?}", full_path);
                             if let Ok(mut resp) = req.into_status_response(404) {
                                resp.write(b"Metadata not found or could not be generated")?;
                                resp.flush()?;
                                resp.release();
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!("Error loading manifest for metadata request {:?}: {}", parent_folder, e);
                 if let Ok(mut resp) = req.into_status_response(500) {
                    resp.write(b"Failed to retrieve file metadata")?;
                    resp.flush()?;
                    resp.release();
                }
            }
        }
    }

    Ok(())
}

fn handle_system_request(
    req: Request<&mut EspHttpConnection>,
    wifi_handle: Arc<Mutex<BlockingWifi<esp_idf_svc::wifi::EspWifi<'static>>>>,
    mount_point: Arc<PathBuf>,
) -> Result<(), anyhow::Error> {
    info!("Handling System API request");

    let wifi = wifi_handle.lock().map_err(|e| anyhow::anyhow!("Failed to lock wifi handle: {:?}", e))?;

    match system_info::get_system_info(&wifi, &mount_point) {
        Ok(info) => {
            let json_response = json!(info).to_string();
             if let Ok(mut resp) = req.into_response(200, Some("OK"), &[("Content-Type", "application/json")]) {
                resp.write(json_response.as_bytes())?;
                resp.flush()?;
                resp.release();
            }
        }
        Err(e) => {
            error!("Failed to get system info: {}", e);
             if let Ok(mut resp) = req.into_status_response(500) {
                resp.write(b"Failed to retrieve system information")?;
                resp.flush()?;
                resp.release();
            }
        }
    }

    Ok(())
}
