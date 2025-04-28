use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    http::server::EspHttpServer,
    nvs::EspDefaultNvsPartition,
    wifi::{self, AccessPointConfiguration, AuthMethod, BlockingWifi, EspWifi},
};

fn main() -> anyhow::Result<()> {
    use std::fs::{read_dir, File};
    use std::io::{Read, Seek, Write};

    use esp_idf_svc::fs::fatfs::Fatfs;
    use esp_idf_svc::hal::gpio;
    use esp_idf_svc::hal::prelude::*;
    use esp_idf_svc::hal::sd::{
        mmc::SdMmcHostConfiguration, mmc::SdMmcHostDriver, SdCardConfiguration, SdCardDriver,
    };
    use esp_idf_svc::io::vfs::MountedFatfs;
    use esp_idf_svc::log::EspLogger;

    use log::info;

    // Initialize ESP-IDF
    esp_idf_svc::sys::link_patches();
    EspLogger::initialize_default();

    // Take peripherals
    let peripherals = Peripherals::take()?;
    let pins = peripherals.pins;

    // Initialize SD card
    let sd_card_driver = SdCardDriver::new_mmc(
        SdMmcHostDriver::new_1bit(
            peripherals.sdmmc1,
            pins.gpio3,
            pins.gpio1,
            pins.gpio2,
            None::<gpio::AnyIOPin>,
            None::<gpio::AnyIOPin>,
            &SdMmcHostConfiguration::new(),
        )?,
        &{
            let mut config = SdCardConfiguration::new();
            config.speed_khz = 40000;
            config
        },
    )?;

    // Mount the SD card
    let fatfs = Fatfs::new_sdcard(0, sd_card_driver)?;
    let mounted_fatfs = MountedFatfs::mount(fatfs, "/sdcard", 4)?;
    info!("SD card mounted at /sdcard");

    // Initialize system event loop and NVS
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    // Initialize WiFi in AP mode
    let wifi_driver = EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?;
    let mut wifi = BlockingWifi::wrap(wifi_driver, sys_loop.clone())?;

    let ap_config = AccessPointConfiguration {
        ssid: "ESP32-WEB-DAV".try_into().unwrap(),
        password: "12345678".try_into().unwrap(),
        auth_method: AuthMethod::WPA2Personal,
        channel: 6,
        ..Default::default()
    };

    wifi.set_configuration(&wifi::Configuration::AccessPoint(ap_config))?;
    wifi.start()?;
    wifi.wait_netif_up()?;
    info!("WiFi Access Point started, SSID: ESP32-WEB-DAV");

    // Initialize HTTP server
    let mut server_config = esp_idf_svc::http::server::Configuration::default();
    server_config.uri_match_wildcard = true;
    let server = EspHttpServer::new(&server_config)?;

    // Create a reference-counted handler to store all our components
    struct AppResources {
        wifi: BlockingWifi<EspWifi<'static>>,
        server: EspHttpServer<'static>,
        mounted_fatfs: MountedFatfs<Fatfs<SdCardDriver<SdMmcHostDriver<'static>>>>,
        sys_loop: EspSystemEventLoop,
    }

    // Store everything in a thread-safe, reference-counted container
    let resources = Arc::new(Mutex::new(AppResources {
        wifi,
        server,
        mounted_fatfs,
        sys_loop,
    }));

    // Setup HTTP handlers
    let resources_clone = resources.clone();
    if let Ok(mut locked_resources) = resources_clone.lock() {
        locked_resources
            .server
            .fn_handler("/*", esp_idf_svc::http::Method::Get, move |req| {
                let root_path = PathBuf::from("/sdcard");
                let path = req.uri();
                let full_path = root_path.join(path.trim_start_matches('/'));

                if !full_path.exists() {
                    if let Ok(mut resp) = req.into_response(404, None, &[]) {
                        resp.flush();
                        resp.release();
                    }
                    return Ok(());
                }

                if full_path.is_dir() {
                    // Simple directory listing
                    let mut content = String::new();
                    if let Ok(entries) = fs::read_dir(&full_path) {
                        for entry in entries.flatten() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                            if is_dir {
                                content.push_str(&format!("{}/\n", name));
                            } else {
                                content.push_str(&format!("{}\n", name));
                            }
                        }
                    }

                    if let Ok(mut resp) =
                        req.into_response(200, None, &[("Content-Type", "text/plain")])
                    {
                        resp.write(content.as_bytes());
                        resp.flush();
                        resp.release();
                    }
                } else {
                    // File content
                    match fs::File::open(&full_path) {
                        Ok(mut file) => {
                            let mut buffer = Vec::new();
                            let content_type =
                                match full_path.extension().and_then(|ext| ext.to_str()) {
                                    Some("txt") => "text/plain",
                                    Some("html") => "text/html",
                                    Some("jpg") | Some("jpeg") => "image/jpeg",
                                    Some("png") => "image/png",
                                    _ => "application/octet-stream",
                                };

                            if file.read_to_end(&mut buffer).is_ok() {
                                if let Ok(mut resp) =
                                    req.into_response(200, None, &[("Content-Type", content_type)])
                                {
                                    resp.write(&buffer).unwrap();
                                    resp.flush();
                                    resp.release();
                                }
                            } else {
                                if let Ok(mut resp) = req.into_status_response(500) {
                                    resp.flush();
                                    resp.release();
                                }
                            }
                        }
                        Err(_) => {
                            if let Ok(mut resp) = req.into_status_response(404) {
                                resp.flush();
                                resp.release();
                            }
                        }
                    }
                }

                Ok::<_, String>(())
            })?;
    }

    // Leak the resources to ensure they live for the entire program
    let leaked_resources = Box::leak(Box::new(resources));

    // Keep the program running with periodic status updates
    info!("Server running and stable. Press Ctrl+C to stop.");
    loop {
        // Periodically log that we're still running
        info!("WebDAV server still active on ESP32-WEB-DAV network");
        thread::sleep(Duration::from_secs(60));

        // Optional: Check WiFi status and reconnect if needed
        if let Ok(mut locked_resources) = leaked_resources.lock() {
            if !locked_resources.wifi.is_connected()? {
                info!("WiFi disconnected, attempting to reconnect...");
                locked_resources.wifi.connect()?;
            }
        }
    }
}
