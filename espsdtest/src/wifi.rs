use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::ipv4::{
    ClientConfiguration as IpClientConfiguration, Configuration as IpConfiguration,
    DHCPClientSettings,
};

use esp_idf_svc::netif::{EspNetif, NetifConfiguration, NetifStack};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration,
    Configuration as WifiConfiguration, EspWifi, WifiDriver,
};
use log::{info, warn};

use crate::config::{WIFI_CHANNEL, WIFI_PASSWORD, WIFI_SSID};

pub fn connect_sta(
    wifi: Arc<Mutex<BlockingWifi<EspWifi<'static>>>>,
    ssid: &str,
    password: &str,
) -> anyhow::Result<()> {
    let mut wifi = wifi
        .lock()
        .map_err(|e| anyhow::anyhow!("Cannot lock wifi {e:?}"))?;
    let sta_config = ClientConfiguration {
        ssid: ssid.try_into().unwrap(),
        bssid: None,
        auth_method: AuthMethod::WPA2Personal,
        password: password.try_into().unwrap(),
        channel: None,
        ..Default::default()
    };

    wifi.set_configuration(&WifiConfiguration::Client(sta_config))?;
    wifi.start()?;
    wifi.connect()?;
    wifi.wait_netif_up()?;

    Ok(())
}

pub fn connect_ap(
    wifi: Arc<Mutex<BlockingWifi<EspWifi<'static>>>>,
    ssid: &str,
    password: &str,
) -> anyhow::Result<()> {
    let mut wifi = wifi
        .lock()
        .map_err(|e| anyhow::anyhow!("Cannot lock wifi {e:?}"))?;

    let ap_config = AccessPointConfiguration {
        ssid: ssid.try_into().unwrap(),
        password: password.try_into().unwrap(),
        auth_method: AuthMethod::WPA2Personal,
        channel: WIFI_CHANNEL,
        ..Default::default()
    };

    wifi.set_configuration(&WifiConfiguration::AccessPoint(ap_config))?;
    wifi.start()?;
    wifi.wait_netif_up()?;

    Ok(())
}

pub fn configure_wifi(
    modem: esp_idf_svc::hal::modem::WifiModem,
    sys_loop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
) -> anyhow::Result<BlockingWifi<EspWifi<'static>>> {
    let wifi = WifiDriver::new(modem, sys_loop.clone(), Some(nvs))?;

    let mut wifi = EspWifi::wrap_all(
        wifi,
        // Note that setting a custom hostname can be used with any network adapter, not just Wifi
        // I.e. that would work with Eth as well, because DHCP is an L3 protocol
        EspNetif::new_with_conf(&NetifConfiguration {
            ip_configuration: Some(IpConfiguration::Client(IpClientConfiguration::DHCP(
                DHCPClientSettings {
                    hostname: Some("personal_cloud".try_into().unwrap()),
                },
            ))),
            ..NetifConfiguration::wifi_default_client()
        })?,
        EspNetif::new(NetifStack::Ap)?,
    )?;

    let wifi = BlockingWifi::wrap(wifi, sys_loop)?;
    Ok(wifi)
}

pub fn disconnect(wifi: Arc<Mutex<BlockingWifi<EspWifi<'static>>>>) -> anyhow::Result<()> {
    info!("Try to Disconnect");
    let mut wifi = wifi
        .lock()
        .map_err(|e| anyhow::anyhow!("Cannot lock wifi {e:?}"))?;
    // Attempt to disconnect if in STA mode, but ignore errors if not connected or not in STA mode.
    // The subsequent calls will ensure WiFi is turned off regardless.
    if let Err(e) = wifi.disconnect() {
        warn!("Ignoring error during wifi.disconnect() (might not be in STA mode or connected): {}", e);
    }
    wifi.set_configuration(&WifiConfiguration::None)?;
    wifi.stop()?;
    info!("WiFi disconnected and stopped successfully.");
    Ok(())
}

// Function to get current IP information and connection status
pub fn get_wifi_status(
    wifi: &BlockingWifi<EspWifi<'static>>,
) -> anyhow::Result<(std::net::Ipv4Addr, bool, bool)> {
    let config = wifi.get_configuration()?;
    let is_connected = wifi.is_connected()?;

    match &config {
        WifiConfiguration::Client(client_config) => {
            let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
            Ok((ip_info.ip, is_connected, false))
        }
        WifiConfiguration::None => Ok((Ipv4Addr::new(0, 0, 0, 0), false, false)),
        WifiConfiguration::AccessPoint(access_point_configuration) => {
            let ip_info = wifi.wifi().ap_netif().get_ip_info()?;
            Ok((ip_info.ip, is_connected, true))
        }
        WifiConfiguration::Mixed(client_configuration, access_point_configuration) => {
            let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
            Ok((ip_info.ip, is_connected, false))
        }
    }
}
