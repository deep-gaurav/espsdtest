use std::ffi::CString;
use std::str::FromStr;
use std::path::Path;
use esp_idf_svc::sys::{esp_vfs_fat_info,  esp_err_t, ESP_OK};
use log::info;

use crate::config::SD_CARD_MOUNT_POINT;

// Struct to hold system information
#[derive(Debug, serde::Serialize)]
pub struct SystemInfo {
    pub health: String,
    pub ip_address: String,
    pub wifi_status: String,
    pub total_space_bytes: u64,
    pub free_space_bytes: u64,
}

// Get storage space information
pub fn get_storage_space() -> anyhow::Result<(u64, u64)> {
    let mut total_bytes = 0;
    let mut free_bytes = 0;
    let mount_point_cstr = CString::from_str(SD_CARD_MOUNT_POINT)?;

    // Use unsafe block to call the C function
    let ret: esp_err_t = unsafe {
        esp_vfs_fat_info(mount_point_cstr.as_ptr(), &mut total_bytes, &mut free_bytes)
    };

    if ret != ESP_OK {
        return Err(anyhow::anyhow!("Failed to get FATFS info: {}", ret));
    }

    Ok((total_bytes , free_bytes ))
}

// Get system information
pub fn get_system_info(
    wifi: &esp_idf_svc::wifi::BlockingWifi<esp_idf_svc::wifi::EspWifi<'static>>,
) -> anyhow::Result<SystemInfo> {
    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
    let ip_address = ip_info.ip.to_string();
    let wifi_status = if wifi.is_connected()? {
        "Connected".to_string()
    } else {
        "Disconnected".to_string()
    };

    info!("Get storage space");
    let (total_space, free_space) = get_storage_space()?;

    Ok(SystemInfo {
        health: "ok".to_string(), // Basic health status
        ip_address,
        wifi_status,
        total_space_bytes: total_space,
        free_space_bytes: free_space,
    })
}
