use std::{
    fs::{self, File},
    io::{Read, Seek, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    http::server::{Configuration, EspHttpConnection, EspHttpServer},
    wifi::BlockingWifi,
};

use crate::{
    config::{AppConfig, MAX_UPLOAD_SIZE, SERVER_PORT},
    storage::get_content_type,
};

// We need to make sure our struct is Send + Sync
pub struct AppResources<'a> {
    pub wifi: BlockingWifi<esp_idf_svc::wifi::EspWifi<'a>>,
    pub server: EspHttpServer<'a>,
    pub mounted_fatfs: esp_idf_svc::io::vfs::MountedFatfs<
        esp_idf_svc::fs::fatfs::Fatfs<
            esp_idf_svc::hal::sd::SdCardDriver<esp_idf_svc::hal::sd::mmc::SdMmcHostDriver<'a>>,
        >,
    >,
    pub sys_loop: EspSystemEventLoop,
    pub config: AppConfig,
}

// Allow resources to be sent across threads
unsafe impl<'a> Send for AppResources<'a> {}
unsafe impl<'a> Sync for AppResources<'a> {}
const STACK_SIZE: usize = 1024*138;

pub fn setup_http_server() -> anyhow::Result<EspHttpServer<'static>> {
    let mut server_config = Configuration{
        stack_size:STACK_SIZE,
        ..Default::default()
    };
    server_config.uri_match_wildcard = true;
    server_config.http_port = SERVER_PORT;
    
    Ok(EspHttpServer::new(&server_config)?)
}

pub fn register_handlers(
    resources: Arc<Mutex<AppResources<'static>>>,
) -> anyhow::Result<()> {
    // Extract what we need outside the handlers to avoid pointer issues
    let config = {
        let locked_resources = resources.lock().map_err(|e|anyhow::anyhow!("{e:?}"))?;
        locked_resources.config.clone()
    };
    
    // Store the root path in an Arc to make it thread-safe
    let root_path = Arc::new(config.root_path);
    let chunk_size = config.chunk_size;
    
    if let Ok(mut locked_resources) = resources.lock() {
        // Handler for GET requests - Directory listing and file download
        let get_root_path = root_path.clone();
        locked_resources
            .server
            .fn_handler("/*", esp_idf_svc::http::Method::Get, move |req| {
                handle_get_request(req, get_root_path.clone(), chunk_size)
            })?;
        
        // Handler for POST requests - File upload
        let post_root_path = root_path.clone();
        locked_resources
            .server
            .fn_handler("/*", esp_idf_svc::http::Method::Post, move |req| {
                handle_post_request(req, post_root_path.clone())
            })?;
    }
    
    Ok(())
}

fn handle_get_request(
    req: esp_idf_svc::http::server::Request<&mut EspHttpConnection>,
    root_path: Arc<PathBuf>,
    chunk_size: usize,
) -> Result<(), anyhow::Error> {
    let path = req.uri();
    let full_path = root_path.join(path.trim_start_matches('/'));

    if !full_path.exists() {
        if let Ok(mut resp) = req.into_response(404, Some("Not Found"), &[]) {
            resp.write(b"File or directory not found")?;
            resp.flush()?;
            resp.release();
        }
        return Ok(());
    }

    if full_path.is_dir() {
        // Simple directory listing
        let mut content = String::from("<!DOCTYPE html><html><head><title>Directory Listing</title></head><body><h1>Directory Listing</h1><ul>");
        
        // Add parent directory link if not at root
        if path != "/" {
            let path = PathBuf::from(path);
            let parent_path = path.parent().unwrap_or_else(|| std::path::Path::new("/"));
            content.push_str(&format!("<li><a href=\"{}\">.. (Parent Directory)</a></li>", parent_path.display()));
        }
        
        if let Ok(entries) = fs::read_dir(&full_path) {
            let mut dirs = Vec::new();
            let mut files = Vec::new();
            
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                let entry_path = format!("{}/{}", path.trim_end_matches('/'), name);
                
                if is_dir {
                    dirs.push(format!("<li><a href=\"{}\">{}/</a></li>", entry_path, name));
                } else {
                    files.push(format!("<li><a href=\"{}\">{}</a></li>", entry_path, name));
                }
            }
            
            // Sort and append directories first, then files
            dirs.sort();
            files.sort();
            content.push_str(&dirs.join(""));
            content.push_str(&files.join(""));
        }
        
        content.push_str("</ul><hr><p>ESP32 WebDAV Server</p></body></html>");

        if let Ok(mut resp) = req.into_response(200, Some("OK"), &[("Content-Type", "text/html")]) {
            resp.write(content.as_bytes())?;
            resp.flush()?;
            resp.release();
        }
    } else {
        // File content with chunked downloading
        match File::open(&full_path) {
            Ok(mut file) => {
                let content_type = get_content_type(&full_path);
                let file_size = file.metadata().map(|m| m.len()).unwrap_or(0);
                
                // Get range header if present
                let range_header = req.header("Range").unwrap_or("").to_string();
                
                if range_header.starts_with("bytes=") {
                    // Handle range request
                    handle_range_request(req, &mut file, file_size, content_type, &range_header, chunk_size)?;
                } else {
                    // Handle full file request with chunking
                    if let Ok(mut resp) = req.into_response(
                        200, 
                        Some("OK"),
                        &[
                            ("Content-Length", &format!("{file_size}")),
                            ("Content-Type", content_type),
                            ("Accept-Ranges", "bytes"),
                        ]
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
                                    log::error!("Error reading file: {}", e);
                                    break;
                                }
                            }
                        }
                        resp.release();
                    }
                }
            }
            Err(e) => {
                log::error!("Error opening file {}: {}", full_path.display(), e);
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
    req: esp_idf_svc::http::server::Request<&mut EspHttpConnection>,
    file: &mut File,
    file_size: u64,
    content_type: &str,
    range_header: &str,
    chunk_size: usize,
) -> Result<(), anyhow::Error> {
    // Parse range header (format: "bytes=start-end")
    let range_part = range_header.trim_start_matches("bytes=");
    let (start, end) = if let Some((start_str, end_str)) = range_part.split_once('-') {
        let start = start_str.parse::<u64>().unwrap_or(0);
        let end = if end_str.is_empty() {
            file_size - 1
        } else {
            end_str.parse::<u64>().unwrap_or(file_size - 1)
        };
        (start, std::cmp::min(end, file_size - 1))
    } else {
        (0, file_size - 1)
    };
    
    let content_length = end - start + 1;
    let content_range = format!("bytes {}-{}/{}", start, end, file_size);
    
    // Seek to the requested position
    if let Err(e) = file.seek(std::io::SeekFrom::Start(start)) {
        log::error!("Error seeking file: {}", e);
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
        ]
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
                        buffer.resize(bytes_remaining as usize, 0);
                    }
                    // log::info!("Wrote 4kb, next");
                }
                Err(e) => {
                    log::error!("Error reading file: {}", e);
                    break;
                }
            }
        }
        resp.release();
    }
    
    Ok(())
}

fn handle_post_request(
    mut req: esp_idf_svc::http::server::Request<&mut EspHttpConnection>,
    root_path: Arc<PathBuf>,
) -> Result<(), anyhow::Error> {
    log::info!("Received request for post");
    let path = req.uri().to_string();
    let full_path = root_path.join(path.trim_start_matches('/'));
    
    log::info!("File path: {:?}", full_path);
    // Create parent directories if they don't exist
    if let Some(parent) = full_path.parent() {
        if !parent.exists() {
            if let Err(e) = fs::create_dir_all(parent) {
                log::error!("Failed to create directory structure: {}", e);
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
        if let Ok(mut resp) = req.into_status_response(413) {
            resp.write(b"File too large")?;
            resp.flush()?;
            resp.release();
        }
        return Ok(());
    }

    log::info!("Create file {:?}",full_path);
    
    // Create or open file for writing
    match File::create(&full_path) {
        Ok(mut file) => {
            let mut buffer = vec![0; 1024*128]; // 4KB buffer for reading chunks
            let mut total_bytes = 0;
            
            // Read request body in chunks
            loop {
                match req.read(&mut buffer) {
                    Ok(0) => break, // End of request
                    Ok(bytes_read) => {
                        if let Err(e) = file.write_all(&buffer[..bytes_read]) {
                            log::error!("Error writing to file: {}", e);
                            if let Ok(mut resp) = req.into_status_response(500) {
                                resp.write(b"Error writing file to storage")?;
                                resp.flush()?;
                                resp.release();
                            }
                            return Ok(());
                        }
                        total_bytes += bytes_read;
                    }
                    Err(e) => {
                        log::error!("Error reading request body: {}", e);
                        break;
                    }
                }
            }
            
            if let Err(e) = file.flush() {
                log::error!("Error flushing file: {}", e);
            }
            
            log::info!("File uploaded successfully: {} ({} bytes)", full_path.display(), total_bytes);
            
            if let Ok(mut resp) = req.into_response(201, Some("Created"), &[("Location", &path)]) {
                resp.write(format!("File uploaded successfully ({} bytes)", total_bytes).as_bytes())?;
                resp.flush()?;
                resp.release();
            }
        }
        Err(e) => {
            log::error!("Failed to create file: {}", e);
            if let Ok(mut resp) = req.into_status_response(500) {
                resp.write(b"Failed to create file on storage")?;
                resp.flush()?;
                resp.release();
            }
        }
    }
    
    Ok(())
}