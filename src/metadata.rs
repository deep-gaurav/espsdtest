use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write, Seek, SeekFrom},
    path::{Path, PathBuf},
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

// Define the structure for a folder's manifest
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FolderManifest {
    // Using a HashMap for quick lookup by name
    pub entries: HashMap<String, FileMetadata>,
}

impl FolderManifest {
    // Create a new empty manifest
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    // Load manifest from a binary file
    // Uses double buffering by checking for a .tmp file
    pub fn load_from_file(path: &Path) -> anyhow::Result<Self> {
        let manifest_path = path.join(".manifest.bin");
        let temp_manifest_path = path.join(".manifest.bin.tmp");

        let file_to_load = if temp_manifest_path.exists() {
            info!("Found temporary manifest file, attempting to load from {:?}", temp_manifest_path);
            temp_manifest_path
        } else {
            manifest_path
        };

        if !file_to_load.exists() {
            info!("Manifest file not found at {:?}, creating new manifest.", file_to_load);
            return Ok(Self::new());
        }

        let mut file = File::open(file_to_load)?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;

        // Deserialize the manifest from binary
        let manifest: FolderManifest = bincode::deserialize(&buffer)?;
        info!("Manifest loaded successfully from {:?}", path);
        Ok(manifest)
    }

    // Save manifest to a binary file using double buffering
    pub fn save_to_file(&self, path: &Path) -> anyhow::Result<()> {
        let manifest_path = path.join(".manifest.bin");
        let temp_manifest_path = path.join(".manifest.bin.tmp");

        // Serialize the manifest to binary
        let encoded: Vec<u8> = bincode::serialize(self)?;

        // Write to a temporary file first
        let mut temp_file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_manifest_path)?;

        temp_file.write_all(&encoded)?;
        temp_file.flush()?;
        drop(temp_file); // Ensure the temporary file is closed

        // Rename the temporary file to the final manifest file
        fs::rename(&temp_manifest_path, &manifest_path)?;
        info!("Manifest saved successfully to {:?}", manifest_path);

        Ok(())
    }

    // Add or update a file metadata entry
    pub fn add_or_update_entry(&mut self, metadata: FileMetadata) {
        self.entries.insert(metadata.name.clone(), metadata);
    }

    // Remove a file metadata entry
    pub fn remove_entry(&mut self, name: &str) {
        self.entries.remove(name);
    }

    // Get metadata for a specific entry
    pub fn get_entry(&self, name: &str) -> Option<&FileMetadata> {
        self.entries.get(name)
    }
}

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
    // Cache of loaded manifests, keyed by folder path
    manifest_cache: Mutex<HashMap<PathBuf, Arc<Mutex<FolderManifest>>>>,
    root_path: PathBuf,
}

impl ManifestManager {
    pub fn new(root_path: PathBuf) -> Self {
        Self {
            manifest_cache: Mutex::new(HashMap::new()),
            root_path,
        }
    }

    // Get or load the manifest for a given folder path
    pub fn get_or_load_manifest(&self, folder_path: &Path) -> anyhow::Result<Arc<Mutex<FolderManifest>>> {
        let mut cache = self.manifest_cache.lock().map_err(|e| anyhow::anyhow!("Failed to lock manifest cache: {:?}", e))?;

        if let Some(manifest) = cache.get(folder_path) {
            return Ok(Arc::clone(manifest));
        }

        // Manifest not in cache, load it
        let manifest = FolderManifest::load_from_file(folder_path)?;
        let manifest_arc = Arc::new(Mutex::new(manifest));
        cache.insert(folder_path.to_path_buf(), Arc::clone(&manifest_arc));

        Ok(manifest_arc)
    }

    // Save the manifest for a given folder path
    pub fn save_manifest(&self, folder_path: &Path) -> anyhow::Result<()> {
        let cache = self.manifest_cache.lock().map_err(|e| anyhow::anyhow!("Failed to lock manifest cache: {:?}", e))?;

        if let Some(manifest_arc) = cache.get(folder_path) {
            let manifest = manifest_arc.lock().map_err(|e| anyhow::anyhow!("Failed to lock manifest: {:?}", e))?;
            manifest.save_to_file(folder_path)?;
            Ok(())
        } else {
            Err(anyhow::anyhow!("Manifest for {:?} not found in cache", folder_path))
        }
    }

    // Add or update an entry in the manifest for a given file path
    pub fn add_or_update_entry(&self, file_path: &Path) -> anyhow::Result<()> {
        let parent_folder = file_path.parent().ok_or_else(|| anyhow::anyhow!("Invalid file path: {:?}", file_path))?;
        let file_name = file_path.file_name().ok_or_else(|| anyhow::anyhow!("Invalid file name: {:?}", file_path))?.to_string_lossy().to_string();

        let manifest_arc = self.get_or_load_manifest(parent_folder)?;
        let mut manifest = manifest_arc.lock().map_err(|e| anyhow::anyhow!("Failed to lock manifest: {:?}", e))?;

        let is_dir = file_path.is_dir();
        let size = if is_dir { 0 } else { fs::metadata(file_path)?.len() };
        let mtime = get_mtime(file_path)?;
        let crc32 = if is_dir { 0 } else { calculate_crc32(file_path)? };

        let metadata = FileMetadata {
            name: file_name,
            size,
            mtime,
            crc32,
            is_dir,
        };

        manifest.add_or_update_entry(metadata);
        self.save_manifest(parent_folder)?;

        Ok(())
    }

    // Remove an entry from the manifest for a given file path
    pub fn remove_entry(&self, file_path: &Path) -> anyhow::Result<()> {
        let parent_folder = file_path.parent().ok_or_else(|| anyhow::anyhow!("Invalid file path: {:?}", file_path))?;
        let file_name = file_path.file_name().ok_or_else(|| anyhow::anyhow!("Invalid file name: {:?}", file_path))?.to_string_lossy().to_string();

        let manifest_arc = self.get_or_load_manifest(parent_folder)?;
        let mut manifest = manifest_arc.lock().map_err(|e| anyhow::anyhow!("Failed to lock manifest: {:?}", e))?;

        manifest.remove_entry(&file_name);
        self.save_manifest(parent_folder)?;

        Ok(())
    }
}
