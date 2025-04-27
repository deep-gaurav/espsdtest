use std::path::PathBuf;

use dav_server::{fakels::FakeLs, localfs::LocalFs, DavHandler};
use esp_idf_svc::{eventloop::EspSystemEventLoop, hal::task::block_on, http::server::EspHttpServer, nvs::EspDefaultNvsPartition, wifi::{self, AccessPointConfiguration, AuthMethod, BlockingWifi, EspWifi}};

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

    esp_idf_svc::sys::link_patches();

    EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;
    let pins = peripherals.pins;

    let sd_card_driver = SdCardDriver::new_mmc(
        // => Data width = 4 bits
        // SdMmcHostDriver::new_slot1_4bits(
        //     peripherals.sdmmc1,
        //     pins.gpio15,
        //     pins.gpio14,
        //     pins.gpio2,
        //     pins.gpio4,
        //     pins.gpio12,
        //     pins.gpio13,
        //     None::<gpio::AnyIOPin>,
        //     None::<gpio::AnyIOPin>,
        //     &SdMmcHostConfiguration::new(),
        // )?,
        // => Data width = 1 bit
        // Comment out the above configuration and uncomment this block
        // if you have connected only the d0 pin
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

    // Keep it around or else it will be dropped and unmounted
    let _mounted_fatfs = MountedFatfs::mount(Fatfs::new_sdcard(0, sd_card_driver)?, "/sdcard", 4)?;

    info!("SD card mounted at /sdcard");

    // ============== Setup WiFi AP =================
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let mut wifi = BlockingWifi::wrap(EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?, sys_loop)?;

    let ap_config = AccessPointConfiguration {
        ssid: "ESP32-WEB-DAV".try_into().unwrap(),
        password: "12345678".try_into().unwrap(),
        auth_method: AuthMethod::WPA2Personal,
        channel: 6,
        ..Default::default()
    };

    wifi.set_configuration(&wifi::Configuration::AccessPoint(ap_config))?;
    wifi.start()?;
    wifi.connect()?;

    info!("WiFi Access Point started, SSID: ESP32-WEB-DAV");

    // ============== Start WebDAV Server =================

    let dav_server = DavHandler::builder()
    .filesystem(LocalFs::new(PathBuf::from("/sdcard"), false, false, false))
    .locksystem(FakeLs::new())
    .build_handler();

    
    block_on(async {
    });
    Ok(())
}
