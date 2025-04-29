use anyhow::Result;
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::json;
use std::env;
use std::time::Duration;
use rand::{Rng, distributions::Alphanumeric};

// Define structures matching your API responses (optional but recommended)
#[derive(Deserialize, Debug, PartialEq)]
struct FileMetadata {
    name: String,
    size: u64,
    mtime: u64,
    crc32: u32,
    is_dir: bool,
}

#[derive(Deserialize, Debug, PartialEq)]
struct SystemInfo {
    health: String,
    ip_address: String,
    wifi_status: String,
    total_space_bytes: u64,
    free_space_bytes: u64,
}

// --- Test Configuration ---
fn get_base_url() -> String {
    // Default IP, can be overridden by environment variable ESP_IP
    let ip = env::var("ESP_IP").unwrap_or_else(|_| "192.168.31.202".to_string());
    format!("http://{}", ip)
}

fn get_api_base() -> String {
    format!("{}/api", get_base_url())
}

fn get_files_url() -> String {
    format!("{}/files", get_api_base())
}

fn get_metadata_url() -> String {
    format!("{}/metadata", get_api_base())
}

fn get_system_url() -> String {
    format!("{}/system", get_api_base())
}

// --- Helper Functions ---

fn create_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(15)) // Set a reasonable timeout
        .build()
        .expect("Failed to create reqwest client")
}

// Helper to generate random strings for filenames or content
fn random_string(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

// Helper to ensure cleanup even if asserts fail
struct TestResource {
    client: Client,
    path: String, // Relative path like "test_file.txt" or "test_dir"
}

impl TestResource {
    fn new(client: Client, path: &str) -> Self {
        TestResource { client, path: path.to_string() }
    }

    fn file_url(&self) -> String {
        format!("{}/{}", get_files_url(), self.path)
    }
}

impl Drop for TestResource {
    fn drop(&mut self) {
        println!("Cleaning up resource: {}", self.path);
        // Send DELETE request, ignore errors during cleanup
        let _ = self.client.delete(self.file_url()).send();
    }
}


// --- Test Cases ---

#[test]
fn test_system_info() -> Result<()> {
    let client = create_client();
    let url = get_system_url();
    println!("Testing GET {}", url);

    let resp = client.get(&url).send()?;

    assert_eq!(resp.status(), StatusCode::OK);
    let info: SystemInfo = resp.json()?;
    println!("Received System Info: {:?}", info);

    assert_eq!(info.health, "ok");
    assert_eq!(info.wifi_status, "Connected"); // Assuming wifi is connected
    assert!(!info.ip_address.is_empty());
    assert!(info.total_space_bytes > 0);
    // free space can fluctuate, just check it's present
    assert!(info.free_space_bytes <= info.total_space_bytes);

    Ok(())
}

#[test]
fn test_upload_download_delete_file() -> Result<()> {
    let client = create_client();
    let filename = format!("test_upload_{}.txt", random_string(8));
    let file_content = format!("Hello from test! Random: {}", random_string(20));
    let resource = TestResource::new(client.clone(), &filename); // RAII cleanup
    let file_url = resource.file_url();
    let metadata_url = format!("{}/{}", get_metadata_url(), filename);

    // 1. Upload
    println!("Testing POST {}", file_url);
    let resp = client
        .post(&file_url)
        .body(file_content.clone())
        .send()?;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // 2. Verify Download (GET)
    println!("Testing GET {}", file_url);
    let resp = client.get(&file_url).send()?;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text()?, file_content);

    // 3. Verify Metadata
    println!("Testing GET {}", metadata_url);
    let resp = client.get(&metadata_url).send()?;
    assert_eq!(resp.status(), StatusCode::OK);
    let meta: FileMetadata = resp.json()?;
    println!("Received Metadata: {:?}", meta);
    assert_eq!(meta.name, filename);
    assert_eq!(meta.size, file_content.len() as u64);

    println!("Meta is {meta:#?}");
    assert!(!meta.is_dir);
    assert!(meta.crc32 != 0); // Should have a CRC for a file
    // assert!(meta.mtime > 1600000000); // Check mtime is a reasonable Unix timestamp

    // 4. Delete (Handled by TestResource drop)
    println!("Testing DELETE {}", file_url);
    // Drop is called implicitly here when resource goes out of scope

    drop(resource);

    // 5. Verify Deletion (GET should be 404)
    println!("Verifying DELETE with GET {}", file_url);
    let resp = client.get(&file_url).send()?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    println!("Verifying DELETE with GET {}", metadata_url);
    let resp = client.get(&metadata_url).send()?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);


    Ok(())
}

#[test]
fn test_create_list_delete_directory() -> Result<()> {
    let client = create_client();
    let dirname = format!("test_dir_{}", random_string(8));
    let resource = TestResource::new(client.clone(), &dirname); // RAII cleanup for dir
    let dir_url = resource.file_url(); // Uses /api/files URL structure
    let dir_metadata_url = format!("{}/{}", get_metadata_url(), dirname);
    let parent_metadata_url = format!("{}/",get_metadata_url()); // Metadata of root

    // 1. Create Directory (MKCOL)
    println!("Testing MKCOL {}", dir_url);
    let resp = client.request(reqwest::Method::from_bytes(b"MKCOL")?, &dir_url).send()?;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // 2. Verify Directory Exists (GET Metadata of parent)
    println!("Testing GET {}", parent_metadata_url);
    let resp = client.get(&parent_metadata_url).send()?;
    assert_eq!(resp.status(), StatusCode::OK);
    let entries: Vec<FileMetadata> = resp.json()?;
    assert!(entries.iter().any(|m| m.name == dirname && m.is_dir), "Directory not found in parent metadata listing");

    // 3. Verify Directory Metadata directly
    println!("Testing GET {}", dir_metadata_url);
    let resp = client.get(&dir_metadata_url).send()?;
    assert_eq!(resp.status(), StatusCode::OK);
    let entries: Vec<FileMetadata> = resp.json()?; // Should be empty list for new dir
    assert!(entries.is_empty(), "Newly created directory metadata should be empty list");

    // 4. Verify Directory Listing (GET files URL)
    println!("Testing GET {}", dir_url);
    let resp = client.get(&dir_url).send()?;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = resp.text()?;
    assert!(html.contains("<h1>Directory Listing</h1>"));
    // assert!(html.contains(&format!("href=\"/{}\"", dirname)), "Listing should contain link to parent"); // Check parent link

    // 5. Upload a file into the directory
    let filename_in_dir = format!("file_in_{}.log", random_string(5));
    let file_path_in_dir = format!("{}/{}", dirname, filename_in_dir);
    let file_content = "Log data".to_string();
    let file_resource = TestResource::new(client.clone(), &file_path_in_dir); // Cleanup for file
    let file_url_in_dir = file_resource.file_url();

    println!("Testing POST {}", file_url_in_dir);
    let resp = client.post(&file_url_in_dir).body(file_content.clone()).send()?;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // 6. Verify Listing contains the file
    println!("Testing GET {} (again)", dir_url);
    let resp = client.get(&dir_url).send()?;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = resp.text()?;
    assert!(html.contains(&filename_in_dir), "Directory listing should contain the uploaded file");

    // 7. Verify Metadata contains the file
    println!("Testing GET {} (again)", dir_metadata_url);
    let resp = client.get(&dir_metadata_url).send()?;
    assert_eq!(resp.status(), StatusCode::OK);
    let entries: Vec<FileMetadata> = resp.json()?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, filename_in_dir);
    assert_eq!(entries[0].size, file_content.len() as u64);
    assert!(!entries[0].is_dir);

    // 8. Delete directory (Handled by TestResource drop for 'resource')
    println!("Testing DELETE {}", dir_url);
    drop(resource);

    // Drop is called implicitly here

    // 9. Verify Deletion (GET should be 404)
    println!("Verifying DELETE with GET {}", dir_url);
    let resp = client.get(&dir_url).send()?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    println!("Verifying DELETE with GET {}", dir_metadata_url);
    let resp = client.get(&dir_metadata_url).send()?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Also verify the file inside is gone (though deleting parent should handle this)
    println!("Verifying DELETE with GET {}", file_url_in_dir);
    let resp = client.get(&file_url_in_dir).send()?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);


    Ok(())
}

#[test]
fn test_get_nonexistent_file() -> Result<()> {
    let client = create_client();
    let url = format!("{}/{}", get_files_url(), "does_not_exist_ever.abc");
    println!("Testing GET {}", url);
    let resp = client.get(&url).send()?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[test]
fn test_delete_nonexistent_file() -> Result<()> {
    let client = create_client();
    let url = format!("{}/{}", get_files_url(), "does_not_exist_ever.abc");
     println!("Testing DELETE {}", url);
    let resp = client.delete(&url).send()?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[test]
fn test_mkcol_conflict() -> Result<()> {
    let client = create_client();
    let dirname = format!("test_mkcol_conflict_{}", random_string(8));
    let resource = TestResource::new(client.clone(), &dirname);
    let dir_url = resource.file_url();

    // Create first time
    println!("Testing MKCOL {}", dir_url);
    let resp = client.request(reqwest::Method::from_bytes(b"MKCOL")?, &dir_url).send()?;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Try creating again
    println!("Testing MKCOL {} (again, expecting conflict)", dir_url);
    let resp = client.request(reqwest::Method::from_bytes(b"MKCOL")?, &dir_url).send()?;
    assert_eq!(resp.status(), StatusCode::CONFLICT); // 409 Conflict

    Ok(())
}

#[test]
fn test_upload_to_directory_path_fails() -> Result<()> {
    let client = create_client();
    let dirname = format!("test_upload_conflict_dir_{}", random_string(8));
    let resource = TestResource::new(client.clone(), &dirname);
    let dir_url = resource.file_url();

    // Create directory
    println!("Testing MKCOL {}", dir_url);
    let resp = client.request(reqwest::Method::from_bytes(b"MKCOL")?, &dir_url).send()?;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Try uploading a file *to* the directory path itself
    println!("Testing POST {} (expecting bad request)", dir_url);
    let resp = client.post(&dir_url).body("This should fail").send()?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST); // Should be 400 Bad Request

    Ok(())
}

// TODO: Add tests for Range requests (requires uploading a slightly larger file)
// TODO: Add tests for uploading files with spaces or special characters in names (URL encoding needed)
// TODO: Add tests for deleting non-empty directories (if your `storage::delete_entry` supports it recursively)
