use std::{sync::{Arc, Mutex}, thread, time::Duration};

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    log::EspLogger,
    nvs::EspDefaultNvsPartition,
};
use storage::test_sd_speed;

mod config;
mod server;
mod storage;
mod wifi;

use crate::{
    config::AppConfig,
    server::setup_http_server,
    storage::setup_sd_card,
    wifi::setup_wifi_ap,
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
    // Initialize WiFi in AP mode
    let wifi = setup_wifi_ap(peripherals.modem, sys_loop.clone(), nvs)?;
    log::info!("WiFi Access Point started, SSID: {}", config::WIFI_SSID);

    // Initialize HTTP server
    let server = setup_http_server()?;

    // Create app resources and configure handlers
    let app_config = AppConfig {
        root_path: "/sdcard".into(),
        chunk_size: 8192, // 8KB chunks for file transfer
    };

    // Store everything in a thread-safe, reference-counted container
    let resources = Arc::new(Mutex::new(server::AppResources {
        wifi,
        server,
        mounted_fatfs,
        sys_loop,
        config: app_config,
    }));

    // Setup HTTP handlers
    server::register_handlers(resources.clone())?;

    // Leak the resources to ensure they live for the entire program
    let leaked_resources = Box::leak(Box::new(resources));

    // Keep the program running with periodic status updates
    log::info!("Server running. Access via http://192.168.4.1");
    loop {
        // Periodically log that we're still running
        log::info!("WebDAV server active on {} network", config::WIFI_SSID);
        thread::sleep(Duration::from_secs(60));

        // Check WiFi status and reconnect if needed
        if let Ok(mut locked_resources) = leaked_resources.lock() {
            if !locked_resources.wifi.is_connected()? {
                log::info!("WiFi disconnected, attempting to reconnect...");
                locked_resources.wifi.connect()?;
            }
        }
    }
}