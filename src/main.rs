use std::{sync::{Arc, Mutex}, thread, time::Duration, path::PathBuf};

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    log::EspLogger,
    nvs::EspDefaultNvsPartition,
};
use storage::test_sd_speed;
use wifi::configure_wifi;

mod config;
mod server;
mod storage;
mod wifi;
mod metadata; // New module
mod system_info; // New module

use crate::{
    config::{AppConfig, DEFAULT_ROOT_FOLDER},
    server::setup_http_server,
    storage::setup_sd_card,
    wifi::setup_wifi_ap,
    metadata::ManifestManager, // Import ManifestManager
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
    // TODO: Decide between AP and Station mode based on configuration
    let wifi = configure_wifi(peripherals.modem, sys_loop.clone(), nvs)?;
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

    let wifi = Arc::new(Mutex::new(wifi));
    // Store everything in a thread-safe, reference-counted container
    let resources = Arc::new(Mutex::new(server::AppResources {
        wifi,
        server,
        mounted_fatfs,
        sys_loop,
        config: app_config,
        manifest_manager, // Add manifest manager to resources
    }));

    // Setup HTTP handlers
    server::register_handlers(resources.clone())?;

    // Leak the resources to ensure they live for the entire program
    // This is necessary because the HTTP server and WiFi run in the background
    let leaked_resources = Box::leak(Box::new(resources));

    // Keep the program running with periodic status updates
    log::info!("Server running. Access via http://192.168.4.1"); // TODO: Get actual IP
    loop {
        // Periodically log that we're still running
        // log::info!("Cloud server active"); // Avoid excessive logging

        // Check WiFi status and reconnect if needed
        // This part might need adjustment depending on your WiFi mode (AP vs Station)
        // In AP mode, it's less likely to disconnect spontaneously.
        // In Station mode, you'd definitely want reconnection logic.
        // For now, keep the basic check.
        if let Ok(mut locked_resources) = leaked_resources.lock() {
             match locked_resources.wifi.lock().map_err(|e| anyhow::anyhow!("Failed to acquire WiFi lock: {:?}", e))?.is_connected() {
                Ok(connected) => {
                    if !connected {
                        log::warn!("WiFi disconnected, attempting to reconnect...");
                        // Attempt to reconnect - configure_wifi might need to be adapted for reconnection
                        // For a simple example, we'll just log the state.
                        // A proper reconnection would involve calling connect() on the wifi instance.
                        // locked_resources.wifi.connect()?; // Uncomment and handle if needed
                    }
                },
                Err(e) => {
                    log::error!("Error checking WiFi status: {}", e);
                }
            }
        } else {
            log::error!("Failed to acquire resources lock in main loop");
        }


        thread::sleep(Duration::from_secs(60));
    }
}
