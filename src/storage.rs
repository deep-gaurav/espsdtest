use esp_idf_svc::{
    fs::fatfs::Fatfs,
    hal::{
        gpio,
        sd::{
            mmc::SdMmcHostConfiguration, mmc::SdMmcHostDriver, SdCardConfiguration, SdCardDriver,
        },
    },
    io::vfs::MountedFatfs,
};
use log::info;

use crate::config::{SD_CARD_MOUNT_POINT, SD_CARD_SPEED_KHZ};

pub fn test_sd_speed() -> anyhow::Result<(f64,f64)> {
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::time::Instant;

    let test_file_path = format!("{}/testfile.bin", SD_CARD_MOUNT_POINT);

    // Create 100MB buffer (heap-allocated)
    let buffer_size = 1024 * 64; // 1MB
    let mut buffer = vec![0u8; buffer_size];

    // Fill buffer with some pattern
    for (i, byte) in buffer.iter_mut().enumerate() {
        *byte = (i % 256) as u8;
    }

    // Write test
    let start_write = Instant::now();
    let mut file = File::create(&test_file_path)?;
    for _ in 0..100 {
        file.write_all(&buffer)?;
    }
    file.flush()?;
    let duration_write = start_write.elapsed();
    let write_speed = (100 * buffer_size) as f64 / duration_write.as_secs_f64() / 1_000_000.0; // MB/s
    info!("SD card write speed: {:.2} MB/s", write_speed);
    drop(file);
    // Read test
    let start_read = Instant::now();
    let mut file = File::open(&test_file_path)?;
    file.seek(SeekFrom::Start(0))?;
    let mut read_buffer = vec![0u8; buffer_size];
    loop {
        let bytes_read = file.read(&mut read_buffer)?;
        if bytes_read == 0 {
            break;
        }
    }
    let duration_read = start_read.elapsed();
    let read_speed = (100 * buffer_size) as f64 / duration_read.as_secs_f64() / 1_000_000.0; // MB/s
    info!("SD card read speed: {:.2} MB/s", read_speed);

    // Cleanup
    std::fs::remove_file(&test_file_path)?;
    Ok((read_speed,write_speed))
}

pub fn setup_sd_card(
    sdmmc1: esp_idf_svc::hal::sd::mmc::SDMMC0,
    cmd_pin: esp_idf_svc::hal::gpio::Gpio3,
    clk_pin: esp_idf_svc::hal::gpio::Gpio1,
    d0_pin: esp_idf_svc::hal::gpio::Gpio2,
    d1_pin: esp_idf_svc::hal::gpio::Gpio5,
    d2_pin: esp_idf_svc::hal::gpio::Gpio6,
    d3_pin: esp_idf_svc::hal::gpio::Gpio4,
) -> anyhow::Result<MountedFatfs<Fatfs<SdCardDriver<SdMmcHostDriver<'static>>>>> {
    // Initialize SD card
    let sd_card_driver = SdCardDriver::new_mmc(
        SdMmcHostDriver::new_4bits(
            sdmmc1,
            cmd_pin,
            clk_pin,
            d0_pin,
            d1_pin,
            d2_pin,
            d3_pin,
            None::<gpio::AnyIOPin>,
            None::<gpio::AnyIOPin>,
            &SdMmcHostConfiguration::new(),
        )?,
        &{
            let mut config = SdCardConfiguration::new();
            config.speed_khz = SD_CARD_SPEED_KHZ;
            config
        },
    )?;

    info!("Created 4 bit mmc");

    // Mount the SD card
    let fatfs = Fatfs::new_sdcard(0, sd_card_driver)?;
    let mounted_fatfs = MountedFatfs::mount(fatfs, SD_CARD_MOUNT_POINT, 4)?;

    Ok(mounted_fatfs)
}

pub fn get_content_type(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("txt") => "text/plain",
        Some("html") | Some("htm") => "text/html",
        Some("css") => "text/css",
        Some("js") => "application/javascript",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("pdf") => "application/pdf",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}
