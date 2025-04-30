use anyhow::Result;
use reqwest::Client; // Use async client
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::json;
use std::env;
use std::time::Duration;
use rand::{Rng, distributions::Alphanumeric};
use tokio::sync::OnceCell;
use tokio::time;

// --- BLE Imports and Constants (from ble_tests.rs) ---
use btleplug::api::{Central, Manager as _, Peripheral, ScanFilter, WriteType, Characteristic};
use btleplug::platform::{Manager};
use futures::stream::StreamExt;
use uuid::{uuid, Uuid};
use serde_json::Value;

const PERSONAL_CLOUD_SERVICE_UUID: Uuid = uuid!("e3aea549-ef01-413d-b981-eb34f12a91b2");
const WIFI_CONFIG_CHARACTERISTIC_UUID: Uuid = uuid!("9ba7e609-1b31-48d0-b3f1-ab725255dac2");
const WIFI_STATUS_CHARACTERISTIC_UUID: Uuid = uuid!("4b19f2c4-10ac-4e68-91d0-4e74a6ceda85");
const TARGET_DEVICE_NAME: &str = "ESP32_Cloud_BLE"; // Use the advertised name

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

// --- Global State for Dynamic IP ---
static ESP_IP_ADDRESS: OnceCell<String> = OnceCell::const_new();

async fn get_base_url() -> Result<String> {
    let ip = ESP_IP_ADDRESS.get_or_try_init(setup_wifi_and_get_ip).await?;
    Ok(format!("http://{}", ip))
}

async fn get_api_base() -> String {
    format!("{}/api", get_base_url().await.expect("Couldnt get wifi base"))
}

async fn get_files_url() -> String {
    format!("{}/files", get_api_base().await)
}

async fn get_metadata_url() -> String {
    format!("{}/metadata", get_api_base().await)
}

async fn get_system_url() -> String {
    format!("{}/system", get_api_base().await)
}

// --- BLE Helper Functions (Adapted from ble_tests.rs) ---

async fn connect_to_device() -> Result<impl Peripheral> {
    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    let central = adapters.into_iter().next().expect("No Bluetooth adapter found");

    println!("Starting scan for {}...", TARGET_DEVICE_NAME);
    central.start_scan(ScanFilter::default()).await?;
    time::sleep(Duration::from_secs(5)).await; // Scan for a few seconds

    let peripherals = central.peripherals().await?;
    for p in peripherals {
        if let Some(properties) = p.properties().await? {
            let local_name = properties.local_name.clone().unwrap_or_default();
            let advertised_services = properties.services;

            if local_name.contains(TARGET_DEVICE_NAME) || advertised_services.contains(&PERSONAL_CLOUD_SERVICE_UUID) {
                 println!("Found target device: {}. Connecting...", local_name);
                p.connect().await?;
                println!("Connected to device: {}", local_name);
                central.stop_scan().await?;
                return Ok(p);
            }
        }
    }

    central.stop_scan().await?;
    Err(anyhow::anyhow!("Target device '{}' not found", TARGET_DEVICE_NAME))
}

async fn get_wifi_status(p: &impl Peripheral, char: &Characteristic) -> Result<Value> {
    // Retry reading status as it might take time to update after config write
    for attempt in 1..=5 {
        println!("Reading WiFi Status (Attempt {})...", attempt);
        match p.read(char).await {
            Ok(status_data) => {
                let status_string = String::from_utf8_lossy(&status_data);
                println!("Raw WiFi Status: {}", status_string);
                match serde_json::from_str(&status_string) {
                    Ok(status_json) => return Ok(status_json),
                    Err(e) => println!("Failed to parse status JSON: {}. Retrying...", e),
                }
            },
            Err(e) => println!("Failed to read WiFi status: {}. Retrying...", e),
        }
        time::sleep(Duration::from_secs(2)).await;
    }
     Err(anyhow::anyhow!("Failed to read valid WiFi status after multiple attempts"))
}

// --- Test Setup Function ---
async fn setup_wifi_and_get_ip() -> Result<String> {
    println!("--- Starting BLE Wi-Fi Setup ---");
    let peripheral = connect_to_device().await?;
    peripheral.discover_services().await?;

    let characteristics = peripheral.characteristics();
    let wifi_config_char = characteristics.iter()
        .find(|c| c.uuid == WIFI_CONFIG_CHARACTERISTIC_UUID)
        .ok_or_else(|| anyhow::anyhow!("WiFi Config Characteristic not found"))?;
    let wifi_status_char = characteristics.iter()
        .find(|c| c.uuid == WIFI_STATUS_CHARACTERISTIC_UUID)
        .ok_or_else(|| anyhow::anyhow!("WiFi Status Characteristic not found"))?;

    // Configure as Station (STA) - Connect to specific WiFi
    println!("Configuring ESP32 to connect to AirFiber-5G via BLE...");
    let sta_config = json!({
        "mode": "Station",
        "ssid": "AirFiber-5G",
        "password": "ajayjyoti@8271" // Ensure this is correct!
    });
    let config_str = sta_config.to_string();
    println!("Writing WiFi Config: {}", config_str);
    peripheral.write(wifi_config_char, config_str.as_bytes(), WriteType::WithResponse).await?;
    println!("Write successful. Waiting for ESP32 to connect to WiFi (up to 30s)...");

    // Wait and check status until connected or timeout
    let connect_timeout = Duration::from_secs(30);
    let start_time = time::Instant::now();
    let mut ip_address = String::new();

    while start_time.elapsed() < connect_timeout {
        time::sleep(Duration::from_secs(5)).await; // Check every 5 seconds
        let sta_status = get_wifi_status(&peripheral, wifi_status_char).await?;
        println!("Current WiFi Status: {:?}", sta_status);
        if sta_status["status"] == "Connected" {
            if let Some(ip) = sta_status["ip_address"].as_str() {
                if ip != "0.0.0.0" && !ip.is_empty() {
                    ip_address = ip.to_string();
                    println!("Successfully connected to WiFi. IP Address: {}", ip_address);
                    break;
                }
            }
        }
    }

    peripheral.disconnect().await?;
    println!("--- BLE Disconnected ---");

    if ip_address.is_empty() {
        Err(anyhow::anyhow!("Failed to connect to WiFi and get IP address within timeout"))
    } else {
        Ok(ip_address)
    }
}

// --- Helper Functions ---

fn create_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(20)) // Increased timeout slightly
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

// Helper for resource cleanup (now using async)
struct TestResource {
    client: Client,
    path: String, // Relative path like "test_file.txt" or "test_dir"
}

impl TestResource {
    fn new(client: Client, path: &str) -> Self {
        TestResource { client, path: path.to_string() }
    }

    async fn file_url(&self) -> String {
        format!("{}/{}", get_files_url().await, self.path)
    }

    // Explicit async cleanup instead of Drop
    async fn cleanup(&self) -> Result<()> {
        println!("Cleaning up resource: {}", self.path);
        // Send DELETE request, ignore errors during cleanup
        let resp = self.client.delete(self.file_url().await).send().await;
        if let Err(e) = resp {
            println!("Warning: Cleanup failed for {}: {}", self.path, e);
        }
        Ok(())
    }
}

// --- Test Cases ---

#[tokio::test]
async fn test_system_info() -> Result<()> {
    let client = create_client();
    let base_url = get_base_url().await?; // Get dynamic base URL
    let url = format!("{}/api/system", base_url);
    println!("Testing GET {}", url);

    let resp = client.get(&url).send().await?;

    assert_eq!(resp.status(), StatusCode::OK);
    let info: SystemInfo = resp.json().await?;
    println!("Received System Info: {:?}", info);

    assert_eq!(info.health, "ok");
    assert_eq!(info.wifi_status, "Connected"); // Assuming wifi is connected
    assert!(!info.ip_address.is_empty());
    assert!(info.total_space_bytes > 0);
    // free space can fluctuate, just check it's present
    assert!(info.free_space_bytes <= info.total_space_bytes);

    Ok(())
}

#[tokio::test]
async fn test_upload_download_delete_file() -> Result<()> {
    let client = create_client();
    let filename = format!("test_upload_{}.txt", random_string(8));
    let file_content = format!("Hello from test! Random: {}", random_string(20));
    let resource = TestResource::new(client.clone(), &filename); // RAII cleanup
    let base_files_url = format!("{}/api/files", get_base_url().await?); // Use dynamic base
    let file_url = format!("{}/{}", base_files_url, filename);
    let metadata_url = format!("{}/{}", get_metadata_url().await, filename);

    // 1. Upload
    println!("Testing POST {}", file_url);
    let resp = client
        .post(&file_url)
        .body(file_content.clone())
        .send().await?;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // 2. Verify Download (GET)
    println!("Testing GET {}", file_url);
    let resp = client.get(&file_url).send().await?;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text().await?, file_content);

    // 3. Verify Metadata
    println!("Testing GET {}", metadata_url);
    let resp = client.get(&metadata_url).send().await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let meta: FileMetadata = resp.json().await?;
    println!("Received Metadata: {:?}", meta);
    assert_eq!(meta.name, filename);
    assert_eq!(meta.size, file_content.len() as u64);

    println!("Meta is {meta:#?}");
    assert!(!meta.is_dir);
    assert!(meta.crc32 != 0); // Should have a CRC for a file
    // assert!(meta.mtime > 1600000000); // Check mtime is a reasonable Unix timestamp

    // 4. Delete (Explicitly call cleanup)
    println!("Testing DELETE {}", file_url);
    resource.cleanup().await?;

    // 5. Verify Deletion (GET should be 404)
    println!("Verifying DELETE with GET {}", file_url);
    let resp = client.get(&file_url).send().await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    println!("Verifying DELETE with GET {}", metadata_url);
    let resp = client.get(&metadata_url).send().await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);


    Ok(())
}

#[tokio::test]
async fn test_create_list_delete_directory() -> Result<()> {
    let client = create_client();
    let dirname = format!("test_dir_{}", random_string(8));
    let resource = TestResource::new(client.clone(), &dirname); // RAII cleanup for dir

    let base_url = get_base_url().await?;
    let base_files_url = format!("{}/api/files", base_url);
    let base_metadata_url = format!("{}/api/metadata", base_url);
    let dir_url = format!("{}/{}", base_files_url, dirname); // Uses /api/files URL structure
    let dir_metadata_url = format!("{}/{}", base_metadata_url, dirname);
    let parent_metadata_url = format!("{}/", base_metadata_url); // Metadata of root

    // 1. Create Directory (MKCOL)
    println!("Testing MKCOL {}", dir_url);
    let resp = client.request(reqwest::Method::from_bytes(b"MKCOL")?, &dir_url).send().await?;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // 2. Verify Directory Exists (GET Metadata of parent)
    println!("Testing GET {}", parent_metadata_url);
    let resp = client.get(&parent_metadata_url).send().await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let entries: Vec<FileMetadata> = resp.json().await?;
    assert!(entries.iter().any(|m| m.name == dirname && m.is_dir), "Directory not found in parent metadata listing");

    // 3. Verify Directory Metadata directly
    println!("Testing GET {}", dir_metadata_url);
    let resp = client.get(&dir_metadata_url).send().await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let entries: Vec<FileMetadata> = resp.json().await?; // Should be empty list for new dir
    assert!(entries.is_empty(), "Newly created directory metadata should be empty list");

    // 4. Verify GET on directory via /api/files/ is not allowed
    println!("Testing GET {}", dir_url);
    let resp = client.get(&dir_url).send().await?;
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED, "GET on a directory via /api/files/ should return 405");

    // 5. Upload a file into the directory
    let filename_in_dir = format!("file_in_{}.log", random_string(5));
    let file_path_in_dir = format!("{}/{}", dirname, filename_in_dir);
    let file_content = "Log data".to_string();
    let file_resource = TestResource::new(client.clone(), &file_path_in_dir); // Cleanup for file
    let file_url_in_dir = format!("{}/{}", base_files_url, file_path_in_dir);

    println!("Testing POST {}", file_url_in_dir);
    let resp = client.post(&file_url_in_dir).body(file_content.clone()).send().await?;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // 6. Verify Metadata now contains the file
    println!("Testing GET {} (to verify file presence)", dir_metadata_url);
    let resp = client.get(&dir_metadata_url).send().await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let entries: Vec<FileMetadata> = resp.json().await?;
    assert!(entries.iter().any(|m| m.name == filename_in_dir && !m.is_dir), "Uploaded file not found in directory metadata listing");

    println!("Testing GET {} (again)", dir_metadata_url);
    let resp = client.get(&dir_metadata_url).send().await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let entries: Vec<FileMetadata> = resp.json().await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, filename_in_dir);
    assert_eq!(entries[0].size, file_content.len() as u64);
    assert!(!entries[0].is_dir);

    // 8. Delete directory (Handled by TestResource drop for 'resource')
    println!("Testing DELETE {}", dir_url);
    resource.cleanup().await?; // Explicit cleanup for dir
    file_resource.cleanup().await?; // Explicit cleanup for file

    // 9. Verify Deletion (GET should be 404)
    println!("Verifying DELETE with GET {}", dir_url);
    let resp = client.get(&dir_url).send().await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    println!("Verifying DELETE with GET {}", dir_metadata_url);
    let resp = client.get(&dir_metadata_url).send().await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Also verify the file inside is gone (though deleting parent should handle this)
    println!("Verifying DELETE with GET {}", file_url_in_dir);
    let resp = client.get(&file_url_in_dir).send().await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);


    Ok(())
}

#[tokio::test]
async fn test_get_nonexistent_file() -> Result<()> {
    let client = create_client();
    let base_files_url = format!("{}/api/files", get_base_url().await?);
    let url = format!("{}/{}", base_files_url, "does_not_exist_ever.abc");
    println!("Testing GET {}", url);
    let resp = client.get(&url).send().await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn test_delete_nonexistent_file() -> Result<()> {
    let client = create_client();
    let base_files_url = format!("{}/api/files", get_base_url().await?);
    let url = format!("{}/{}", base_files_url, "does_not_exist_ever.abc");
     println!("Testing DELETE {}", url);
    let resp = client.delete(&url).send().await?;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn test_mkcol_conflict() -> Result<()> {
    let client = create_client();
    let dirname = format!("test_mkcol_conflict_{}", random_string(8));
    let resource = TestResource::new(client.clone(), &dirname);
    let base_files_url = format!("{}/api/files", get_base_url().await?);
    let dir_url = format!("{}/{}", base_files_url, dirname);

    // Create first time
    println!("Testing MKCOL {}", dir_url);
    let resp = client.request(reqwest::Method::from_bytes(b"MKCOL")?, &dir_url)
        .send().await?;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Try creating again
    println!("Testing MKCOL {} (again, expecting conflict)", dir_url);
    let resp = client.request(reqwest::Method::from_bytes(b"MKCOL")?, &dir_url).send().await?;
    assert_eq!(resp.status(), StatusCode::CONFLICT); // 409 Conflict

    // Cleanup
    resource.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn test_upload_to_directory_path_fails() -> Result<()> {
    let client = create_client();
    let dirname = format!("test_upload_conflict_dir_{}", random_string(8));
    let resource = TestResource::new(client.clone(), &dirname);
    let base_files_url = format!("{}/api/files", get_base_url().await?);
    let dir_url = format!("{}/{}", base_files_url, dirname);

    // Create directory
    println!("Testing MKCOL {}", dir_url);
    let resp = client.request(reqwest::Method::from_bytes(b"MKCOL")?, &dir_url).send().await?;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Try uploading a file *to* the directory path itself
    println!("Testing POST {} (expecting bad request)", dir_url);
    let resp = client.post(&dir_url).body("This should fail").send().await?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST); // Should be 400 Bad Request

    // Cleanup
    resource.cleanup().await?;
    Ok(())
}

// TODO: Add tests for Range requests (requires uploading a slightly larger file)
// TODO: Add tests for uploading files with spaces or special characters in names (URL encoding needed)
// TODO: Add tests for deleting non-empty directories (if your `storage::delete_entry` supports it recursively)
