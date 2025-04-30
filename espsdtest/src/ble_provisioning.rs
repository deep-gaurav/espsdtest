use anyhow::Result;
use esp32_nimble::{uuid128, BLEAdvertisementData, BLECharacteristic, BLEDevice, NimbleProperties};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::wifi::BlockingWifi; // Assuming you still use esp-idf-svc for WiFi
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::{self, json};
use std::path::PathBuf; // Assuming you still use PathBuf
use std::sync::{Arc, Mutex};
use std::time::Duration;

// Assume these are defined elsewhere as in your original file
// use crate::{metadata::{ManifestManager, FileMetadata}, system_info::SystemInfo};
// use crate::wifi::{BleWifiConfig, WifiMode, get_wifi_status}; // Assuming WiFi handling is in wifi.rs
// use crate::config::{WIFI_SSID, WIFI_PASSWORD}; // Assuming config is in config.rs

// --- UUIDs for our custom service and characteristics ---
// Use the same UUIDs as in your original ble_provisioning.rs
pub const PERSONAL_CLOUD_SERVICE_UUID: &'static str = "e3aea549-ef01-413d-b981-eb34f12a91b2";
pub const WIFI_CONFIG_CHARACTERISTIC_UUID: &'static str = "9ba7e609-1b31-48d0-b3f1-ab725255dac2";
pub const WIFI_STATUS_CHARACTERISTIC_UUID: &'static str = "4b19f2c4-10ac-4e68-91d0-4e74a6ceda85";
pub const DIR_LIST_REQUEST_CHARACTERISTIC_UUID: &'static str =
    "b6c4219e-242d-4999-a099-657bc8c7e288";
pub const DIR_LIST_RESPONSE_CHARACTERISTIC_UUID: &'static str =
    "f2f0a3b6-8c2e-4d6d-affd-89880b6cb844";
pub const METADATA_REQUEST_CHARACTERISTIC_UUID: &'static str =
    "7c603293-2e90-471f-9abd-03228b7ff1ae";
pub const METADATA_RESPONSE_CHARACTERISTIC_UUID: &'static str =
    "fbae61a1-9427-4274-8c1f-1f5d6b002f8d";
pub const SYSTEM_INFO_CHARACTERISTIC_UUID: &'static str = "42e0c238-6581-48e6-8371-afc57afd9d74";

// Assume data structures are defined as in your original file
// #[derive(Serialize, Deserialize, Debug)]
// pub struct BleWifiConfig { ... }
// #[derive(Serialize, Deserialize, Debug, PartialEq)]
// pub enum WifiMode { ... }
// #[derive(Serialize, Deserialize, Debug)]
// pub struct BleDirListRequest { ... }
// #[derive(Serialize, Deserialize, Debug)]
// pub struct BleMetadataRequest { ... }

/// Struct to hold the BLE provisioning server state and resources.
/// 'a lifetime is used because BLEDevice::take() returns a static reference,
/// and we store a mutable reference to it.
pub struct BleProvisioningServer<'a> {
    ble_device: &'a mut esp32_nimble::BLEDevice,
    // Shared resources, protected by Arc<Mutex<>> for access from closures
    wifi: Arc<Mutex<BlockingWifi<esp_idf_svc::wifi::EspWifi<'static>>>>,
    root_path: Arc<PathBuf>,
    // manifest_manager: Arc<ManifestManager>, // Assuming ManifestManager is still used
}

impl<'a> BleProvisioningServer<'a> {
    /// Creates a new instance of the BLE provisioning server.
    pub fn new(
        ble_device: &'a mut esp32_nimble::BLEDevice,
        wifi: Arc<Mutex<BlockingWifi<esp_idf_svc::wifi::EspWifi<'static>>>>,
        root_path: Arc<PathBuf>,
        // manifest_manager: Arc<ManifestManager>,
    ) -> Self {
        Self {
            ble_device,
            wifi,
            root_path,
            // manifest_manager,
        }
    }

    /// Starts the BLE GATT server and advertising.
    pub fn start(&mut self) -> Result<()> {
        info!("Starting BLE Provisioning Server");

        // Get advertising and server instances from the BLE device
        let ble_advertising = self.ble_device.get_advertising();
        let server = self.ble_device.get_server();

        // Set up connection event handlers
        server.on_connect(|server, desc| {
            info!("Client connected: {:?}", desc);
            // Update connection parameters
            server
                .update_conn_params(desc.conn_handle(), 24, 48, 0, 60)
                .unwrap();

            // If multi-connect is supported and max connections not reached, continue advertising
            if server.connected_count() < (esp_idf_svc::sys::CONFIG_BT_NIMBLE_MAX_CONNECTIONS as _)
            {
                info!("Multi-connect support: start advertising");
                ble_advertising.lock().start().unwrap();
            }
        });

        server.on_disconnect(|_desc, reason| {
            info!("Client disconnected ({:?})", reason);
        });

        // Create the main custom service
        let service = server.create_service(uuid128!(PERSONAL_CLOUD_SERVICE_UUID));
        info!(
            "Created service with UUID: {:?}",
            uuid128!(PERSONAL_CLOUD_SERVICE_UUID)
        );

        // --- Create Characteristics ---

        // WiFi Configuration Characteristic (Write)
        let wifi_config_characteristic = service.lock().create_characteristic(
            uuid128!(WIFI_CONFIG_CHARACTERISTIC_UUID),
            NimbleProperties::WRITE, // Only WRITE property needed as per original
        );
        info!(
            "Created WiFi Config Characteristic with UUID: {:?}",
            uuid128!(WIFI_CONFIG_CHARACTERISTIC_UUID)
        );

        // Clone necessary data for the closure to access shared resources
        let wifi_arc_clone = self.wifi.clone();
        wifi_config_characteristic.lock().on_write(move |args| {
            info!(
                "Received data on WiFi Config characteristic: {:?}",
                args.recv_data()
            );
            let received_data = args.recv_data();

            // Handle WiFi configuration data
            match serde_json::from_slice::<BleWifiConfig>(received_data) {
                Ok(wifi_config) => {
                    info!("Parsed WiFi config: {:?}", wifi_config);

                    // TODO: Implement actual WiFi configuration logic in wifi.rs
                    // This would involve stopping the current WiFi, changing mode/credentials, and starting it again.
                    match wifi_config.mode {
                        WifiMode::Station => {
                            if let (Some(ssid), Some(password)) =
                                (wifi_config.ssid, wifi_config.password)
                            {
                                info!("Attempting to connect to WiFi station: {}", ssid);
                                // Call a function in wifi.rs to configure and connect
                                match crate::wifi::connect_sta(
                                    wifi_arc_clone.clone(),
                                    &ssid,
                                    &password,
                                ) {
                                    Ok(_) => info!("WiFi station connected successfully"),
                                    Err(e) => error!("Failed to connect to WiFi station: {}", e),
                                }
                            } else {
                                error!("SSID or password missing for Station mode");
                            }
                        }
                        WifiMode::AccessPoint => {
                            info!("Attempting to configure WiFi AP");
                            // Call a function in wifi.rs to configure and start AP
                            match crate::wifi::connect_ap(
                                wifi_arc_clone.clone(),
                                crate::config::WIFI_SSID,
                                crate::config::WIFI_PASSWORD,
                            ) {
                                Ok(_) => info!("WiFi AP started successfully"),
                                Err(e) => error!("Failed to start WiFi AP: {}", e),
                            }
                        }
                        WifiMode::Disabled => {
                            info!("Attempting to disable WiFi");
                            // Call a function in wifi.rs to disable WiFi
                            match crate::wifi::disconnect(wifi_arc_clone.clone()) {
                                Ok(_) => info!("WiFi disabled successfully"),
                                Err(e) => error!("Failed to disable WiFi: {}", e),
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(
                        "Failed to parse WiFi config data: {:?} - {}",
                        received_data, e
                    );
                }
            }
        });

        // WiFi Status Characteristic (Read/Notify)
        let wifi_status_characteristic = service.lock().create_characteristic(
            uuid128!(WIFI_STATUS_CHARACTERISTIC_UUID),
            NimbleProperties::READ | NimbleProperties::NOTIFY, // READ and NOTIFY properties
        );
        info!(
            "Created WiFi Status Characteristic with UUID: {:?}",
            uuid128!(WIFI_STATUS_CHARACTERISTIC_UUID)
        );

        let wifi_arc_clone = self.wifi.clone();
        wifi_status_characteristic.lock().on_read({
            move |this, _| {
                info!("Read from WiFi Status characteristic.");
                // Access the cloned Arc and lock the mutex to use the WiFi instance
                let wifi = match wifi_arc_clone.lock() {
                    Ok(wifi) => wifi,
                    Err(e) => {
                        error!("Failed to lock WiFi mutex for read: {}", e);
                        this.set_value(b"Error accessing WiFi status");
                        return; // Return an error message
                    }
                };

                // Retrieve and format WiFi status
                match crate::wifi::get_wifi_status(&wifi) {
                    // Assuming get_wifi_status exists
                    Ok((ip, connected, is_ap)) => {
                        let status = if connected {
                            if is_ap {
                                "AccessPoint"
                            } else {
                                "Connected"
                            }
                        } else {
                            "Disconnected"
                        };
                        let status_str = json!({ "status":  status, "ip_address": ip});
                        info!("Sending WiFi status: {}", status_str);
                        this.set_value(status_str.to_string().as_bytes());
                    }
                    Err(e) => {
                        error!("Failed to get WiFi status for BLE read: {}", e);
                        this.set_value(b"Error getting WiFi status");
                    }
                }
            }
        });
        // For notifications, you'll need a separate mechanism to trigger them, likely in your main loop
        // or in response to WiFi status changes. You'll get a handle to this characteristic
        // and call wifi_status_characteristic.lock().set_value(...).notify();

        // Directory Listing Request Characteristic (Write)
        let dir_list_request_characteristic = service.lock().create_characteristic(
            uuid128!(DIR_LIST_REQUEST_CHARACTERISTIC_UUID),
            NimbleProperties::WRITE, // WRITE property
        );
        info!(
            "Created Dir List Request Characteristic with UUID: {:?}",
            uuid128!(DIR_LIST_REQUEST_CHARACTERISTIC_UUID)
        );

        let root_path_arc_clone = self.root_path.clone();
        // let manifest_manager_arc_clone = self.manifest_manager.clone(); // Assuming ManifestManager is used
        // To send a notification from here, we need access to the Dir List Response characteristic.
        // We can get a handle to the service and then look up the characteristic by UUID.

        dir_list_request_characteristic.lock().on_write({
            let service = service.clone();
            move |args| {
                info!(
                    "Received data on Dir List Request characteristic: {:?}",
                    args.recv_data()
                );
                let received_data = args.recv_data();

                // Handle directory listing request
                match serde_json::from_slice::<BleDirListRequest>(received_data) {
                    Ok(dir_list_req) => {
                        info!("Parsed Dir List Request: {:?}", dir_list_req);
                        // TODO: Implement directory listing logic
                        // Access root_path_arc_clone and potentially manifest_manager_arc_clone

                        // After generating the directory listing data, send it via the Dir List Response characteristic
                        let dir_list_response_char_uuid =
                            uuid128!(DIR_LIST_RESPONSE_CHARACTERISTIC_UUID);

                        esp_idf_svc::hal::task::block_on(async {
                            if let Some(dir_list_response_characteristic) = service
                                .lock()
                                .get_characteristic(dir_list_response_char_uuid)
                                .await
                            {
                                let response_data = b"Directory listing data..."; // Replace with actual data (serialize your directory listing)
                                info!(
                                    "Sending directory listing data ({} bytes).",
                                    response_data.len()
                                );
                                dir_list_response_characteristic
                                    .lock()
                                    .set_value(response_data)
                                    .notify();
                            } else {
                                error!("Dir List Response characteristic not found.");
                            }
                        });
                    }
                    Err(err) => {
                        error!(
                            "Failed to parse Dir List Request data: {:?} - {:?}",
                            received_data, err
                        );
                    }
                }
            }
        });

        // Directory Listing Response Characteristic (Notify)
        // This characteristic's value will be set and notified by the handler of the request characteristic.
        let _dir_list_response_characteristic = service.lock().create_characteristic(
            uuid128!(DIR_LIST_RESPONSE_CHARACTERISTIC_UUID),
            NimbleProperties::READ | NimbleProperties::NOTIFY, // Needs READ for CCCD to be readable, NOTIFY for notifications
        );
        info!(
            "Created Dir List Response Characteristic with UUID: {:?}",
            uuid128!(DIR_LIST_RESPONSE_CHARACTERISTIC_UUID)
        );

        // Metadata Request Characteristic (Write)
        let metadata_request_characteristic = service.lock().create_characteristic(
            uuid128!(METADATA_REQUEST_CHARACTERISTIC_UUID),
            NimbleProperties::WRITE, // WRITE property
        );
        info!(
            "Created Metadata Request Characteristic with UUID: {:?}",
            uuid128!(METADATA_REQUEST_CHARACTERISTIC_UUID)
        );

        let root_path_arc_clone = self.root_path.clone();
        // let manifest_manager_arc_clone = self.manifest_manager.clone(); // Assuming ManifestManager is used
        let service_arc = Arc::new(Mutex::new(service.clone())); // Clone service for access in closure

        metadata_request_characteristic
            .lock()
            .on_write(move |args| {
                info!(
                    "Received data on Metadata Request characteristic: {:?}",
                    args.recv_data()
                );
                let received_data = args.recv_data();

                // Handle metadata request
                match serde_json::from_slice::<BleMetadataRequest>(received_data) {
                    Ok(metadata_req) => {
                        info!("Parsed Metadata Request: {:?}", metadata_req);
                        // TODO: Implement metadata retrieval logic
                        // Access root_path_arc_clone and potentially manifest_manager_arc_clone

                        // After retrieving metadata, send it via the Metadata Response characteristic
                        let metadata_response_char_uuid =
                            uuid128!(METADATA_RESPONSE_CHARACTERISTIC_UUID);

                        // Access the service and find the response characteristic
                        let service = match service_arc.lock() {
                            Ok(service) => service,
                            Err(e) => {
                                error!("Failed to lock service mutex: {}", e);
                                return; // Exit the closure on error
                            }
                        };
                        esp_idf_svc::hal::task::block_on(async {
                            if let Some(metadata_response_characteristic) = service
                                .lock()
                                .get_characteristic(metadata_response_char_uuid)
                                .await
                            {
                                let response_data = b"Metadata data..."; // Replace with actual data (serialize your metadata)
                                info!("Sending metadata data ({} bytes).", response_data.len());
                                metadata_response_characteristic
                                    .lock()
                                    .set_value(response_data)
                                    .notify();
                            } else {
                                error!("Metadata Response characteristic not found.");
                            }
                        });
                    }

                    Err(e) => {
                        error!(
                            "Failed to parse Metadata Request data: {:?} - {}",
                            received_data, e
                        );
                    }
                }
            });

        // Metadata Response Characteristic (Notify)
        let _metadata_response_characteristic = service.lock().create_characteristic(
            uuid128!(METADATA_RESPONSE_CHARACTERISTIC_UUID),
            NimbleProperties::READ | NimbleProperties::NOTIFY, // READ for CCCD, NOTIFY for notifications
        );
        info!(
            "Created Metadata Response Characteristic with UUID: {:?}",
            uuid128!(METADATA_RESPONSE_CHARACTERISTIC_UUID)
        );

        // System Info Characteristic (Read/Notify)
        let system_info_characteristic = service.lock().create_characteristic(
            uuid128!(SYSTEM_INFO_CHARACTERISTIC_UUID),
            NimbleProperties::READ | NimbleProperties::NOTIFY, // READ and NOTIFY properties
        );
        info!(
            "Created System Info Characteristic with UUID: {:?}",
            uuid128!(SYSTEM_INFO_CHARACTERISTIC_UUID)
        );

        let wifi_arc_clone = self.wifi.clone();
        let root_path_arc_clone = self.root_path.clone();
        system_info_characteristic.lock().on_read({
            move |this, _| {
                info!("Read from System Info characteristic.");
                // Access cloned Arcs and lock mutexes
                let wifi = match wifi_arc_clone.lock() {
                    Ok(wifi) => wifi,
                    Err(e) => {
                        error!("Failed to lock WiFi mutex for read: {}", e);
                        this.set_value(b"Error accessing resources");
                        return;
                    }
                };
                let root_path = root_path_arc_clone.as_ref();

                // Retrieve and format system information
                match crate::system_info::get_system_info(&wifi, &root_path) {
                    // Assuming get_system_info exists
                    Ok(system_info) => {
                        let response = serde_json::json!(system_info).to_string();
                        info!("Sending system info: {}", response);
                        this.set_value(response.as_bytes());
                    }
                    Err(e) => {
                        error!("Failed to get system info for BLE read: {}", e);
                        this.set_value(b"Error getting system info");
                    }
                }
            }
        });
        // For notifications, you'll need a separate mechanism to trigger them
        // system_info_characteristic.lock().set_value(...).notify().unwrap();

        // Configure and start advertising
        info!("Configuring BLE advertising");
        ble_advertising.lock().set_data(
            BLEAdvertisementData::new()
                .name("ESP32_Cloud_BLE") // Use the same device name
                .add_service_uuid(uuid128!(PERSONAL_CLOUD_SERVICE_UUID)),
        )?;

        info!("Starting BLE advertising");
        ble_advertising.lock().start()?;

        // Show local GATT server details (optional, for debugging)
        server.ble_gatts_show_local();

        Ok(())
    }

    // You might add methods here to trigger notifications based on state changes
    // For example, a method to notify clients about WiFi status changes
    // This would require storing characteristic instances or handles and connection info in your struct.
    // For simplicity in this conversion, we won't implement this method directly
    // but show how you would call notify() in the main loop or other event handlers.
    // pub fn notify_wifi_status(&self) -> Result<()> {
    //     // Find the WiFi Status characteristic and connected clients
    //     // Set the value and call .notify() for each subscribed client
    //      warn!("notify_wifi_status not implemented in this example structure.");
    //      Ok(())
    // }
}

// --- Dummy structs and enums to make the example compile without the rest of your project ---
// You should remove or replace these with your actual implementations.

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct BleWifiConfig {
    pub ssid: Option<String>,
    pub password: Option<String>,
    pub mode: WifiMode,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
pub enum WifiMode {
    Station,
    AccessPoint,
    Disabled,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct BleDirListRequest {
    pub path: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub struct BleMetadataRequest {
    pub path: String,
}
