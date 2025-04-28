use std::path::PathBuf;

// WiFi configuration
pub const WIFI_SSID: &str = "ESP32-WEB-DAV";
pub const WIFI_PASSWORD: &str = "12345678";
pub const WIFI_CHANNEL: u8 = 6;

// Server configuration
pub const SERVER_PORT: u16 = 80;
pub const MAX_UPLOAD_SIZE: u64 =  10 * 1024 * 1024 * 1024; // 10GB max upload size

// SD Card configuration
pub const SD_CARD_MOUNT_POINT: &str = "/sdcard";
pub const SD_CARD_SPEED_KHZ: u32 = 40000;

// App configuration that can be modified at runtime
#[derive(Clone)]
pub struct AppConfig {
    pub root_path: PathBuf,
    pub chunk_size: usize,
}