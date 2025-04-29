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
use log::info;

use crate::config::{WIFI_CHANNEL, WIFI_PASSWORD, WIFI_SSID};

pub fn setup_wifi_ap(
    modem: esp_idf_svc::hal::modem::Modem,
    sys_loop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
) -> anyhow::Result<BlockingWifi<EspWifi<'static>>> {
    let wifi_driver = EspWifi::new(modem, sys_loop.clone(), Some(nvs))?;
    let mut wifi = BlockingWifi::wrap(wifi_driver, sys_loop)?;

    let ap_config = AccessPointConfiguration {
        ssid: WIFI_SSID.try_into().unwrap(),
        password: WIFI_PASSWORD.try_into().unwrap(),
        auth_method: AuthMethod::WPA2Personal,
        channel: WIFI_CHANNEL,
        ..Default::default()
    };

    wifi.set_configuration(&WifiConfiguration::AccessPoint(ap_config))?;
    wifi.start()?;
    wifi.wait_netif_up()?;

    Ok(wifi)
}

pub fn configure_wifi(
    modem: esp_idf_svc::hal::modem::Modem,
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

    let wifi_configuration = WifiConfiguration::Client(ClientConfiguration {
        ssid: "AirFiber-5G".try_into().unwrap(),
        bssid: None,
        auth_method: AuthMethod::WPA2Personal,
        password: "ajayjyoti@8271".try_into().unwrap(),
        channel: None,
        ..Default::default()
    });
    wifi.set_configuration(&wifi_configuration)?;

    let mut wifi = BlockingWifi::wrap(wifi, sys_loop)?;

    wifi.start()?;
    wifi.connect()?;
    wifi.wait_netif_up()?;

    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;

    info!("Wifi Interface info: {ip_info:?}");

    Ok(wifi)
}
