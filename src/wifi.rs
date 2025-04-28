use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    nvs::EspDefaultNvsPartition,
    wifi::{AccessPointConfiguration, AuthMethod, BlockingWifi, Configuration, EspWifi},
};

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

    wifi.set_configuration(&Configuration::AccessPoint(ap_config))?;
    wifi.start()?;
    wifi.wait_netif_up()?;

    Ok(wifi)
}