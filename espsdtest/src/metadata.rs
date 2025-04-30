use std::{
    // collections::HashMap, // No longer needed
    fs::{self, File, OpenOptions},
    io::{Read, Write, Seek, SeekFrom, BufReader, BufWriter, BufRead},
    path::{Path, PathBuf}, // Added BufReader, BufWriter, BufRead
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use crc::{Crc, CRC_32_CKSUM};
use log::{info, error};
use serde::{Serialize, Deserialize};

// Define the file metadata structure
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileMetadata {
    pub name: String,
    pub size: u64,
    pub mtime: u64, // Modification time as Unix timestamp
    pub crc32: u32,
    pub is_dir: bool,
}

// FolderManifest struct is removed as we operate directly on the file now.

// Helper function to calculate CRC32 of a file
pub fn calculate_crc32(path: &Path) -> anyhow::Result<u32> {
    let mut file = File::open(path)?;

    // Create a CRC-32 instance using the IEEE polynomial
    const CRC_IEEE: Crc<u32> = Crc::<u32>::new(&CRC_32_CKSUM);
    let mut digest = CRC_IEEE.digest(); // Create a digest object

    let mut buffer = [0u8; 4096]; // Read in chunks

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        digest.update(&buffer[..bytes_read]); // Update the digest with the chunk
    }

    Ok(digest.finalize()) // Finalize the digest to get the CRC32 checksum
}

// Helper function to get modification time of a file/directory
pub fn get_mtime(path: &Path) -> anyhow::Result<u64> {
    let metadata = fs::metadata(path)?;
    let mtime = metadata.modified()?;
    let duration_since_epoch = mtime.duration_since(UNIX_EPOCH)?;
    Ok(duration_since_epoch.as_secs())
}

// Struct to manage manifests in memory
pub struct ManifestManager {
    // No longer holds manifest data, just the root path.
    // Mutex might not be strictly necessary anymore if methods are self-contained, but keep for now.
    root_path: PathBuf,
}

impl ManifestManager {
    pub fn new(root_path: PathBuf) -> Self {
        Self {
            // current_manifest: Mutex::new(None), // Removed
            root_path,
        }
    }

    // Save the manifest for a given folder path - No longer needed, replaced by specific operations
    // pub fn save_manifest(&self, folder_path: &Path) -> anyhow::Result<()> { ... }

    // Add or update an entry in the manifest for a given file path
    pub fn add_or_update_entry(&self, file_path: &Path) -> anyhow::Result<()> {
        let is_dir = file_path.is_dir();
        let size = if is_dir { 0 } else { fs::metadata(file_path)?.len() };
        let mtime = get_mtime(file_path)?;
        let crc32 = if is_dir { 0 } else { calculate_crc32(file_path)? };
        let filename = file_path.file_name().ok_or(anyhow::anyhow!("Couldnt get file name"))?;

        let metadata = FileMetadata {
            name: filename.to_string_lossy().to_string(),
            size,
            mtime,
            crc32,
            is_dir,
        };

        let parent_folder = file_path.parent().ok_or_else(|| anyhow::anyhow!("Invalid file path (no parent): {:?}", file_path))?;
        let manifest_path = parent_folder.join(".manifest.jsonl");
        let file_name = metadata.name.clone(); // Use name from calculated metadata

        info!("Adding/updating entry '{}' in manifest {:?}", file_name, manifest_path);

        // This operation requires rewriting the file to ensure atomicity and handle updates.
        // A simpler append-only approach could be used if updates weren't needed or handled differently.
        rewrite_manifest_excluding(&manifest_path, &file_name, Some(&metadata))?;

        info!("Manifest updated successfully for entry '{}'", file_name);
        Ok(())
    }

    // Remove an entry from the manifest for a given file path
    pub fn remove_entry(&self, file_path: &Path) -> anyhow::Result<()> {
        let parent_folder = file_path.parent().ok_or_else(|| anyhow::anyhow!("Invalid file path (no parent): {:?}", file_path))?;
        let file_name = file_path.file_name().ok_or_else(|| anyhow::anyhow!("Invalid file name: {:?}", file_path))?.to_string_lossy().to_string();
        let manifest_path = parent_folder.join(".manifest.jsonl");

        info!("Removing entry '{}' from manifest {:?}", file_name, manifest_path);

        // Removal requires rewriting the file without the specified entry.
        rewrite_manifest_excluding(&manifest_path, &file_name, None)?;

        info!("Manifest updated successfully, removed entry '{}'", file_name);
        Ok(())
    }

    // Get metadata for a specific file by streaming the manifest
    pub fn get_entry_metadata(&self, file_path: &Path) -> anyhow::Result<Option<FileMetadata>> {
        let parent_folder = file_path.parent().ok_or_else(|| anyhow::anyhow!("Invalid file path (no parent): {:?}", file_path))?;
        let file_name = file_path.file_name().ok_or_else(|| anyhow::anyhow!("Invalid file name: {:?}", file_path))?.to_string_lossy().to_string();
        let manifest_path = parent_folder.join(".manifest.jsonl");

        read_manifest_entry(&manifest_path, &file_name)
    }

    // Get all entries for a directory by streaming the manifest
    pub fn get_directory_listing(&self, folder_path: &Path) -> anyhow::Result<Vec<FileMetadata>> {
        let manifest_path = folder_path.join(".manifest.jsonl");
        read_all_manifest_entries(&manifest_path)
    }
}

// --- Helper Functions for Stream-Based Manifest Operations ---

/// Reads a manifest file line by line and returns the metadata for a specific entry name.
fn read_manifest_entry(manifest_path: &Path, entry_name: &str) -> anyhow::Result<Option<FileMetadata>> {
    if !manifest_path.exists() {
        info!("Manifest file not found at {:?}, cannot get entry.", manifest_path);
        return Ok(None);
    }

    let file = File::open(manifest_path)?;
    let reader = BufReader::new(file);

    for (line_num, line_result) in reader.lines().enumerate() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                error!("Error reading line {} from manifest {:?}: {}", line_num + 1, manifest_path, e);
                continue; // Skip malformed lines
            }
        };
        if line.trim().is_empty() {
            continue; // Skip empty lines
        }
        match serde_json::from_str::<FileMetadata>(&line) {
            Ok(metadata) => {
                if metadata.name == entry_name {
                    info!("Found entry '{}' in manifest {:?}", entry_name, manifest_path);
                    return Ok(Some(metadata));
                }
            }
            Err(e) => error!("Failed to parse manifest line {} ('{}'): {}", line_num + 1, line, e),
        }
    }

    info!("Entry '{}' not found in manifest {:?}", entry_name, manifest_path);
    Ok(None)
}

/// Reads a manifest file line by line and returns a Vec of all valid entries.
fn read_all_manifest_entries(manifest_path: &Path) -> anyhow::Result<Vec<FileMetadata>> {
    let mut entries = Vec::new();
    if !manifest_path.exists() {
        info!("Manifest file not found at {:?}, returning empty list.", manifest_path);
        return Ok(entries);
    }

    let file = File::open(manifest_path)?;
    let reader = BufReader::new(file);

    for (line_num, line_result) in reader.lines().enumerate() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                error!("Error reading line {} from manifest {:?}: {}", line_num + 1, manifest_path, e);
                continue; // Skip malformed lines
            }
        };
        if line.trim().is_empty() {
            continue; // Skip empty lines
        }
        match serde_json::from_str::<FileMetadata>(&line) {
            Ok(metadata) => entries.push(metadata),
            Err(e) => error!("Failed to parse manifest line {} ('{}'): {}", line_num + 1, line, e),
        }
    }
    info!("Read {} entries from manifest {:?}", entries.len(), manifest_path);
    Ok(entries)
}

/// Rewrites the manifest file, excluding a specific entry, and optionally appending a new one.
/// Uses a temporary file and rename for atomicity.
fn rewrite_manifest_excluding(manifest_path: &Path, exclude_name: &str, append_entry: Option<&FileMetadata>) -> anyhow::Result<()> {
    let temp_manifest_path = manifest_path.with_extension("jsonl.tmp");

    info!("Rewriting manifest {:?} to temp file {:?}", manifest_path, temp_manifest_path);

    // Open temp file for writing
    let temp_file = OpenOptions::new().write(true).create(true).truncate(true).open(&temp_manifest_path)?;
    let mut writer = BufWriter::new(temp_file);

    // Read original file line by line (if it exists)
    if manifest_path.exists() {
        let file = File::open(manifest_path)?;
        let reader = BufReader::new(file);

        for line_result in reader.lines() {
            let line = line_result?;
            if line.trim().is_empty() { continue; }

            // Try parsing, write if it doesn't match exclude_name
            if let Ok(metadata) = serde_json::from_str::<FileMetadata>(&line) {
                if metadata.name != exclude_name {
                    writer.write_all(line.as_bytes())?;
                    writer.write_all(b"\n")?;
                } else {
                    info!("Excluding entry '{}' during rewrite.", exclude_name);
                }
            } else {
                // Write malformed lines back as they were? Or skip? Skipping for now.
                error!("Skipping malformed line during rewrite: {}", line);
            }
        }
    }

    // Append the new/updated entry if provided
    if let Some(entry_to_append) = append_entry {
        info!("Appending entry '{}' during rewrite.", entry_to_append.name);
        serde_json::to_writer(&mut writer, entry_to_append)?;
        writer.write_all(b"\n")?;
    }

    // Finalize write to temp file
    writer.flush()?;
    drop(writer.into_inner().map_err(|e| e.into_error())?); // Ensure file is closed

    // --- Atomic Rename ---
    // Remove original file before renaming (optional but safer on some systems)
    if manifest_path.exists() {
        fs::remove_file(manifest_path).map_err(|e| {
            error!("Failed to remove original manifest {:?} before rename: {}", manifest_path, e);
            // Attempt cleanup of temp file
            let _ = fs::remove_file(&temp_manifest_path);
            e
        })?;
    }

    // Rename temp to final
    fs::rename(&temp_manifest_path, manifest_path).map_err(|e| {
        error!("Failed to rename temp manifest {:?} to {:?}: {}", temp_manifest_path, manifest_path, e);
        // Attempt cleanup of temp file
        let _ = fs::remove_file(&temp_manifest_path);
        e
    })?;

    info!("Successfully rewrote manifest to {:?}", manifest_path);
    Ok(())
}
