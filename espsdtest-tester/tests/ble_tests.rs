use anyhow::Result;
use btleplug::api::{Central, Manager as _, Peripheral, ScanFilter, WriteType};
use btleplug::platform::{Manager};
use futures::stream::StreamExt;
use std::time::Duration;
use tokio::time::{self, timeout};
use reqwest::Client as HttpClient; // Use the async client
use uuid::{uuid, Uuid};
use serde_json::{json, Value};

// Use the UUIDs defined in your ble_provisioning.rs file
const PERSONAL_CLOUD_SERVICE_UUID: Uuid = uuid!("e3aea549-ef01-413d-b981-eb34f12a91b2");
const WIFI_CONFIG_CHARACTERISTIC_UUID: Uuid = uuid!("9ba7e609-1b31-48d0-b3f1-ab725255dac2");
const WIFI_STATUS_CHARACTERISTIC_UUID: Uuid = uuid!("4b19f2c4-10ac-4e68-91d0-4e74a6ceda85");
const SYSTEM_INFO_CHARACTERISTIC_UUID: Uuid = uuid!("42e0c238-6581-48e6-8371-afc57afd9d74");

// Structure for HTTP System Info response (copied/adapted from api_tests.rs)
#[derive(serde::Deserialize, Debug, PartialEq)]
struct SystemInfo {
    health: String,
    ip_address: String,
    wifi_status: String,
    total_space_bytes: u64,
    free_space_bytes: u64,
}


const TARGET_DEVICE_NAME: &str = "ESP32_Cloud_BLE"; // Use the advertised name

async fn connect_to_device() -> Result<impl Peripheral> {
    let manager = Manager::new().await?;
    let central = manager.adapters().await?.into_iter().next().expect("No Bluetooth adapter found");

    // Start scanning for devices
    println!("Starting scan...");
    central.start_scan(ScanFilter::default()).await?;
    time::sleep(Duration::from_secs(5)).await; // Scan for a few seconds to discover devices

    // Find the peripheral by advertised name or service UUID
    let peripherals = central.peripherals().await?;
    println!("Found {} peripherals during scan.", peripherals.len());
    for p in peripherals {
        let Some(properties) = p.properties().await? else {
            continue;
        };
        
        let local_name = properties.local_name.clone().unwrap_or_default();
        let advertised_services = properties.services;

        println!("Found device: {} ({})", local_name, properties.address);

        // Check if the device name matches OR if it advertises the service UUID
        if local_name.contains(TARGET_DEVICE_NAME) || advertised_services.contains(&PERSONAL_CLOUD_SERVICE_UUID) {
             println!("Connecting to {}...", local_name);
            p.connect().await?;
            println!("Connected to device: {}", local_name);
            central.stop_scan().await?;
            return Ok(p);
        }
    }

    central.stop_scan().await?;
    Err(anyhow::anyhow!("Target device not found"))
}

#[tokio::test]
async fn test_ble_connect_and_discover_services() -> Result<()> {
    let peripheral = connect_to_device().await?;
    peripheral.discover_services().await?;

    let services = peripheral.services();
    println!("Discovered Services: {:?}", services);

    // Check if the personal cloud service is present
    let personal_cloud_service = services.iter().find(|s| s.uuid == PERSONAL_CLOUD_SERVICE_UUID);
    assert!(personal_cloud_service.is_some(), "Personal Cloud Service not found");

    peripheral.disconnect().await?;
    Ok(())
}

#[tokio::test]
#[ignore] // Ignored by default as it requires specific WiFi credentials and network availability
async fn test_ble_wifi_full_cycle() -> Result<()> {
    let peripheral = connect_to_device().await?;
    peripheral.discover_services().await?;

    let characteristics = peripheral.characteristics();
    let wifi_config_char = characteristics.iter()
        .find(|c| c.uuid == WIFI_CONFIG_CHARACTERISTIC_UUID)
        .expect("WiFi Config Characteristic not found");
    let wifi_status_char = characteristics.iter()
        .find(|c| c.uuid == WIFI_STATUS_CHARACTERISTIC_UUID)
        .expect("WiFi Status Characteristic not found");

    // --- Helper Function to Read and Parse WiFi Status ---
    async fn get_wifi_status(p: &impl Peripheral, char: &btleplug::api::Characteristic) -> Result<Value> {
        println!("Reading WiFi Status...");
        let status_data = p.read(char).await?;
        let status_string = String::from_utf8_lossy(&status_data);
        println!("Raw WiFi Status: {}", status_string);
        let status_json: Value = serde_json::from_str(&status_string)?;
        println!("Parsed WiFi Status: {:?}", status_json);
        Ok(status_json)
    }

    // --- Helper Function to Write WiFi Config ---
    async fn set_wifi_config(p: &impl Peripheral, char: &btleplug::api::Characteristic, config: Value) -> Result<()> {
        let config_str = config.to_string();
        println!("Writing WiFi Config: {}", config_str);
        p.write(char, config_str.as_bytes(), WriteType::WithResponse).await?;
        println!("Write successful. Waiting for ESP32 to apply changes...");
        time::sleep(Duration::from_secs(10)).await; // Give ESP time to change mode
        Ok(())
    }

    // 1. Check Initial State (Assuming Disconnected)
    println!("--- Step 1: Checking Initial WiFi State ---");
    let initial_status = get_wifi_status(&peripheral, wifi_status_char).await?;
    assert_eq!(initial_status["status"], "Disconnected", "Initial state should be Disconnected");

    // 2. Configure as Access Point (AP)
    println!("--- Step 2: Configuring as Access Point ---");
    let ap_config = json!({
        "mode": "AccessPoint",
        "ssid": "ESP_Test_AP",
        "password": "testpassword"
    });
    set_wifi_config(&peripheral, wifi_config_char, ap_config).await?;

    // 3. Verify AP State
    println!("--- Step 3: Verifying Access Point State ---");
    let ap_status = get_wifi_status(&peripheral, wifi_status_char).await?;
    assert_eq!(ap_status["status"], "AccessPoint", "Device should be in AP mode");
    let ap_ip = ap_status["ip_address"].as_str().unwrap_or("0.0.0.0");
    assert_ne!(ap_ip, "0.0.0.0", "AP IP address should be valid");
    assert_ne!(ap_ip, "", "AP IP address should not be empty");
    println!("AP Mode Verified. IP: {}", ap_ip);

    // 4. Disconnect (Set Mode to Disabled)
    println!("--- Step 4: Disconnecting WiFi ---");
    let disable_config = json!({ "mode": "Disabled" });
    set_wifi_config(&peripheral, wifi_config_char, disable_config).await?;

    // 5. Verify Disconnected State
    println!("--- Step 5: Verifying Disconnected State ---");
    let disconnected_status = get_wifi_status(&peripheral, wifi_status_char).await?;
    assert_eq!(disconnected_status["status"], "Disconnected", "Device should be Disconnected after disabling");
    println!("WiFi Disconnected Verified.");

    // 6. Configure as Station (STA) - Connect to specific WiFi
    println!("--- Step 6: Configuring as Station ---");
    let sta_config = json!({
        "mode": "Station",
        "ssid": "AirFiber-5G",
        "password": "ajayjyoti@8271"
    });
    // Use a longer delay for station connection attempt
    println!("Writing WiFi Config: {}", sta_config.to_string());
    peripheral.write(wifi_config_char, sta_config.to_string().as_bytes(), WriteType::WithResponse).await?;
    println!("Write successful. Waiting for ESP32 to connect to WiFi...");
    time::sleep(Duration::from_secs(25)).await; // Increased wait time for STA connection

    // 7. Verify Station State via BLE
    println!("--- Step 7: Verifying Station State (via BLE) ---");
    let sta_status = get_wifi_status(&peripheral, wifi_status_char).await?;
    assert_eq!(sta_status["status"], "Connected", "Device should be Connected in Station mode");
    let sta_ip = sta_status["ip_address"].as_str().unwrap_or("0.0.0.0");
    assert_ne!(sta_ip, "0.0.0.0", "Station IP address should be valid");
    assert_ne!(sta_ip, "", "Station IP address should not be empty");
    println!("Station Mode Verified via BLE. IP: {}", sta_ip);

    // 8. Perform HTTP System Info Check
    println!("--- Step 8: Verifying via HTTP Request ---");
    let http_client = HttpClient::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let system_url = format!("http://{}/api/system", sta_ip);

    println!("Making HTTP GET request to: {}", system_url);
    let resp = http_client.get(&system_url).send().await?;

    assert!(resp.status().is_success(), "HTTP request failed with status: {}", resp.status());
    let http_info: SystemInfo = resp.json().await?;
    println!("Received System Info via HTTP: {:?}", http_info);

    assert_eq!(http_info.health, "ok");
    assert_eq!(http_info.wifi_status, "Connected");
    assert_eq!(http_info.ip_address, sta_ip); // Verify IP matches
    assert!(http_info.total_space_bytes > 0);
    assert!(http_info.free_space_bytes <= http_info.total_space_bytes);
    println!("HTTP System Info Verified.");

    // 9. Disconnect BLE
    println!("--- Test Complete: Disconnecting BLE ---");
    peripheral.disconnect().await?;
    Ok(())
}



#[tokio::test]
async fn test_ble_wifi_config_write() -> Result<()> {
    let peripheral = connect_to_device().await?;
    peripheral.discover_services().await?;

    let characteristics = peripheral.characteristics();
    let wifi_config_char = characteristics.iter()
        .find(|c| c.uuid == WIFI_CONFIG_CHARACTERISTIC_UUID)
        .expect("WiFi Config Characteristic not found");

    // Example WiFi configuration data (replace with your actual struct and serialization)
    let wifi_config_data = json!({
        "ssid": "MyAwesomeWiFi",
        "password": "supersecretpassword",
        "mode": "Station" // or "AccessPoint", "Disabled"
    }).to_string();

    println!("Writing WiFi config: {}", wifi_config_data);
    peripheral.write(wifi_config_char, wifi_config_data.as_bytes(), WriteType::WithResponse).await?;
    println!("Write successful");

    peripheral.disconnect().await?;
    Ok(())
}

#[tokio::test]
async fn test_ble_wifi_status_read_and_notify() -> Result<()> {
    let peripheral = connect_to_device().await?;
    peripheral.discover_services().await?;

    let characteristics = peripheral.characteristics();
    let wifi_status_char = characteristics.iter()
        .find(|c| c.uuid == WIFI_STATUS_CHARACTERISTIC_UUID)
        .expect("WiFi Status Characteristic not found");

    // Test Read
    let status_data = peripheral.read(wifi_status_char).await?;
    let status_string = String::from_utf8_lossy(&status_data);
    println!("Read WiFi Status: {}", status_string);
    // Add assertions based on expected status format

    // Test Notify
    peripheral.subscribe(wifi_status_char).await?;
    println!("Subscribed to WiFi Status notifications.");

    let mut notification_stream = peripheral.notifications().await?;

    // Wait for a notification (you might need to trigger a status change on the ESP32)
    if let Some(data) = time::timeout(Duration::from_secs(10), notification_stream.next()).await.ok().flatten() {
        let received_status = String::from_utf8_lossy(&data.value);
        println!("Received WiFi Status Notification: {}", received_status);
        // Add assertions based on expected notification content
    } else {
        println!("No WiFi Status notification received within timeout.");
    }

    peripheral.unsubscribe(wifi_status_char).await?;
    println!("Unsubscribed from WiFi Status notifications.");

    peripheral.disconnect().await?;
    Ok(())
}

#[tokio::test]
async fn test_ble_system_info_read_and_notify() -> Result<()> {
     let peripheral = connect_to_device().await?;
    peripheral.discover_services().await?;

    let characteristics = peripheral.characteristics();
    let system_info_char = characteristics.iter()
        .find(|c| c.uuid == SYSTEM_INFO_CHARACTERISTIC_UUID)
        .expect("System Info Characteristic not found");

    // Test Read
    let info_data = peripheral.read(system_info_char).await?;
    let info_string = String::from_utf8_lossy(&info_data);
    println!("Read System Info: {}", info_string);
    // Add assertions based on expected system info format (likely JSON)

    // Test Notify
    peripheral.subscribe(system_info_char).await?;
    println!("Subscribed to System Info notifications.");

    let mut notification_stream = peripheral.notifications().await?;

    // Wait for a notification (you might need to trigger a system info update on the ESP32)
     if let Some(data) = time::timeout(Duration::from_secs(10), notification_stream.next()).await.ok().flatten() {
        let received_info = String::from_utf8_lossy(&data.value);
        println!("Received System Info Notification: {}", received_info);
        // Add assertions based on expected notification content (likely JSON)
    } else {
        println!("No System Info notification received within timeout.");
    }


    peripheral.unsubscribe(system_info_char).await?;
    println!("Unsubscribed from System Info notifications.");

    peripheral.disconnect().await?;
    Ok(())
}