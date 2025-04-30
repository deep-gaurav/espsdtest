use std::{sync::{Arc, Mutex}, thread, time::Duration, path::PathBuf};

use esp32_nimble::BLEDevice;
use esp_idf_svc::{
     eventloop::EspSystemEventLoop, log::EspLogger, nvs::EspDefaultNvsPartition 
};
use log::{error, info};
use storage::test_sd_speed;
use wifi::configure_wifi;

mod config;
mod server;
mod storage;
mod wifi;
mod metadata;
mod system_info;
mod ble_provisioning; // Import the new module

use crate::{
    config::{AppConfig, DEFAULT_ROOT_FOLDER},
    server::setup_http_server,
    storage::setup_sd_card,
    wifi::setup_wifi_ap,
    metadata::ManifestManager,
    ble_provisioning::BleProvisioningServer, // Import the BLE server struct
};

fn main() -> anyhow::Result<()> {
    // Initialize ESP-IDF
    esp_idf_svc::sys::link_patches();
    EspLogger::initialize_default();

    // Take peripherals
    let peripherals = esp_idf_svc::hal::prelude::Peripherals::take()?;
    let pins = peripherals.pins;

    // Initialize system event loop and NVS
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    // Initialize SD card
    let mounted_fatfs = setup_sd_card(peripherals.sdmmc0, pins.gpio3, pins.gpio1, pins.gpio2,pins.gpio5,pins.gpio6,pins.gpio4)?;
    log::info!("SD card mounted at /sdcard");

    log::info!("Testing speed..");

    let (readspeed,writespeed) = test_sd_speed()?;

    log::info!("Read speed: {readspeed}, Write speed: {writespeed}");

    // Initialize WiFi
    // TODO: Decide between AP and Station mode based on configuration or BLE input
    let (wifi,bluetooth) = peripherals.modem.split();
    let wifi = configure_wifi(wifi, sys_loop.clone(), nvs.clone())?; // Pass nvs to configure_wifi
    log::info!("WiFi configured");

    // Initialize HTTP server with connection limit
    let server = setup_http_server()?;

    // Create app resources and configure handlers
    let root_path = PathBuf::from(DEFAULT_ROOT_FOLDER);
    // Ensure the root directory exists
    if !root_path.exists() {
        log::info!("Creating root directory: {:?}", root_path);
        std::fs::create_dir_all(&root_path)?;
    }

    let app_config = AppConfig {
        root_path: root_path.clone(),
        chunk_size: 8192, // 8KB chunks for file transfer
    };

    // Initialize Manifest Manager
    let manifest_manager = Arc::new(ManifestManager::new(root_path.clone()));

    let wifi_arc = Arc::new(Mutex::new(wifi)); // Wrap wifi in Arc<Mutex>

    
    let ble_device = BLEDevice::take();
    let mut ble_server = BleProvisioningServer::new(ble_device, wifi_arc.clone(), root_path.into());
    ble_server.start()?;
    log::info!("BLE Provisioning Server initialized");


    // Store everything in a thread-safe, reference-counted container
    let resources = Arc::new(Mutex::new(server::AppResources {
        wifi: wifi_arc.clone(), // Use the cloned wifi Arc
        server,
        mounted_fatfs,
        sys_loop,
        config: app_config,
        manifest_manager: manifest_manager.clone(), // Use the cloned manifest_manager Arc
    }));

    // Setup HTTP handlers
    server::register_handlers(resources.clone())?;

    // Leak the resources to ensure they live for the entire program
    // This is necessary because the HTTP server, WiFi, and BLE run in the background
    let leaked_resources = Box::leak(Box::new(resources));
    let leaked_ble_server = Box::leak(Box::new(ble_server)); // Leak the BLE server instance


    // Keep the program running with periodic status updates
    log::info!("Server running. Access via http://192.168.4.1 (or configured IP)"); // TODO: Get actual IP and update via BLE notification
    loop {
        // Periodically check WiFi status and potentially notify via BLE
        if let Ok(locked_resources) = leaked_resources.lock() {
             if let Ok(wifi) = locked_resources.wifi.lock() {
                 match crate::wifi::get_wifi_status(&wifi) {
                    Ok((ip, connected)) => {
                        // You could send a BLE notification here if the status changes
                        // or periodically if a client is subscribed to the WiFi Status characteristic.
                        // This requires accessing the ble_server instance from here.
                        // For now, we'll rely on a client reading the characteristic.
                         info!("WiFi Status: {}, IP: {}", if connected { "Connected" } else { "Disconnected" }, ip);
                    },
                    Err(e) => {
                        error!("Error checking WiFi status: {}", e);
                    }
                 }
             } else {
                 error!("Failed to acquire WiFi lock in main loop for status check");
             }
        } else {
            error!("Failed to acquire resources lock in main loop for status check");
        }


        thread::sleep(Duration::from_secs(10)); // Reduced sleep for more frequent checks
    }
}

