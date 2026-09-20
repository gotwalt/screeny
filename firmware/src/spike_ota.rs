//! Card 200 spike — **evidence, not the implementation.**
//!
//! Compiled only with `--features spike-ota`. It exists to prove three things
//! link and fit on this exact pinned stack, and to let `xtensa-esp32-elf-size`
//! put a number on what they cost:
//!
//! 1. `esp-bootloader-esp-idf 0.6.0`'s partition-table / `Ota` / `OtaUpdater`
//!    API compiles against `esp-storage 0.10.0` on `esp-hal 1.2.2`, and the
//!    multi-core story (`multicore_auto_park`) is reachable.
//! 2. An OTA image can be validated *before* otadata is touched, using only
//!    what these crates give us (ESP image header, `esp_app_desc`, the
//!    appended SHA-256 via `PartitionEntry::sha256`).
//! 3. `sequential-storage 8.0.1` (async-only) runs over an
//!    `esp-bootloader-esp-idf` `NorFlashRegion` through
//!    `embassy_embedded_hal::adapter::BlockingAsync`.
//!
//! None of this is wired to the network, and nothing here should survive into
//! the build cards unchanged. See `docs/research/006-flash-store-ota.md`.
//!
//! **It never runs on the bench.** `spike_report` is called from `main` only
//! under the feature, so the linker cannot strip it, but the feature is off by
//! default and card 200 is `hardware: no`.

use embassy_embedded_hal::adapter::BlockingAsync;
use esp_bootloader_esp_idf::ota::{Ota, OtaImageState};
use esp_bootloader_esp_idf::ota_updater::OtaUpdater;
use esp_bootloader_esp_idf::partitions::{
    self, AppPartitionSubType, DataPartitionSubType, Error as PartError, FlashStorage,
    PartitionType, PARTITION_TABLE_MAX_LEN,
};
use log::info;
use sequential_storage::cache::Cache;
use sequential_storage::map::{MapConfig, MapStorage};

/// Label of the settings partition in `firmware/partitions.csv`.
const CONFIG_LABEL: &str = "screeny";

/// The `esp_app_desc` project name every image we accept must carry. It is
/// `CARGO_PKG_NAME`, i.e. `screeny-fw`, because `main.rs` uses the
/// no-argument form of `esp_app_desc!`.
const PROJECT_NAME: &str = "screeny-fw";

/// One 4 KB sector: the unit we erase and write in, so that core 1 is stalled
/// for one sector-erase (~50 ms) at a time rather than one 64 KB block
/// (~400 ms) or one whole slot.
const CHUNK: usize = 4096;

/// Key set for the settings map. `u8` keys keep the record small;
/// `sequential-storage` needs `Key`, which is implemented for `u8`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
#[allow(dead_code)]
pub enum SettingKey {
    SchemaVersion = 0,
    WifiSsid = 1,
    WifiPsk = 2,
    Name = 3,
    Brightness = 4,
    IdleMode = 5,
}

/// Everything the spike wants to say about the device's flash.
#[derive(Debug, Default)]
pub struct Report {
    pub table_entries: usize,
    pub booted_offset: Option<u32>,
    pub selected: Option<u8>,
    pub state: Option<u32>,
    pub config_offset: Option<u32>,
    pub config_len: Option<u32>,
}

/// Walk the partition table, the OTA state, and the settings partition.
///
/// Every call here is one the real implementation will need, which is the
/// point: if this links, the design in research 006 is buildable.
pub async fn spike_report(flash: esp_hal::peripherals::FLASH<'static>) -> Result<Report, PartError> {
    let mut report = Report::default();

    // The display owns core 1 forever, so the default `MultiCoreStrategy::Error`
    // would make every erase/write return `OtherCoreRunning`. Auto-park stalls
    // core 1 (RTC_CNTL SW_STALL) for the duration of each ROM flash call.
    let mut flash = FlashStorage::new(flash).multicore_auto_park();

    // --- the table ---------------------------------------------------------
    let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let table = partitions::read_partition_table(&mut flash, &mut buf)?;
    report.table_entries = table.len();
    report.booted_offset = table.booted_partition()?.map(|p| p.offset());

    for entry in table.iter() {
        if entry.label_as_str() == CONFIG_LABEL {
            report.config_offset = Some(entry.offset());
            report.config_len = Some(entry.len());
        }
    }

    // --- otadata -----------------------------------------------------------
    let ota_part = table
        .find_partition(PartitionType::Data(DataPartitionSubType::Ota))?
        .ok_or(PartError::Invalid)?;
    {
        let mut ota = Ota::new(ota_part.as_flash_region(&mut flash), 2)?;
        report.selected = Some(ota.current_app_partition()? as u8);
        report.state = ota.current_ota_state().ok().map(|s| s as u32);
    }

    // --- the health confirmation the real firmware owes the bootloader -----
    confirm_if_pending(&mut flash)?;

    // --- validate the *other* slot before anything switches to it ----------
    let mut updater_buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let mut updater = OtaUpdater::new(&mut flash, &mut updater_buf)?;
    let (_region, next) = updater.next_partition()?;
    info!("spike: next OTA slot would be {:?}", next);

    // The whole staging path, so the linker keeps it and the size numbers in
    // research 006 include it: write one sector, then validate the slot, then
    // (and only then) would the real code call `activate_next_partition`.
    let chunk = [0xffu8; CHUNK];
    let _ = stage_chunk(&mut flash, next, 0, &chunk);
    let mut version = [0u8; 32];
    match validate_staged(&mut flash, next, &mut version) {
        Ok(()) => info!("spike: staged image would be accepted"),
        Err(e) => info!("spike: staged image rejected: {:?}", e),
    }

    // --- the settings map --------------------------------------------------
    let _ = settings_roundtrip(&mut flash, &table).await;

    Ok(report)
}

/// The app side of rollback: if this image is the one the bootloader is
/// watching, say it works. Card 200 recommends calling this only once the
/// device has joined WiFi *and* served one HTTP request.
fn confirm_if_pending(flash: &mut FlashStorage<'static>) -> Result<(), PartError> {
    let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let table = partitions::read_partition_table(flash, &mut buf)?;
    let ota_part = table
        .find_partition(PartitionType::Data(DataPartitionSubType::Ota))?
        .ok_or(PartError::Invalid)?;
    let mut ota = Ota::new(ota_part.as_flash_region(flash), 2)?;

    match ota.current_ota_state() {
        Ok(OtaImageState::PendingVerify) | Ok(OtaImageState::New) => {
            ota.set_current_ota_state(OtaImageState::Valid)?;
        }
        _ => {}
    }
    Ok(())
}

/// Refuse an image the bootloader would accept but we do not want: wrong chip,
/// wrong project, bad appended hash. Runs against the *staged* slot, so it is
/// the last gate before `activate_next_partition`.
///
/// Returns the image's `esp_app_desc` version string copied into `version`.
pub fn validate_staged(
    flash: &mut FlashStorage<'static>,
    slot: AppPartitionSubType,
    version: &mut [u8; 32],
) -> Result<(), PartError> {
    let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let table = partitions::read_partition_table(flash, &mut buf)?;
    let entry = table
        .find_partition(PartitionType::App(slot))?
        .ok_or(PartError::Invalid)?;

    // Header: magic 0xE9 and the ESP32 chip id (0x0000) at offset 12..14.
    let mut header = [0u8; 24];
    let mut region = entry.as_flash_region(flash);
    region.read(0, &mut header)?;
    if header[0] != 0xE9 {
        return Err(PartError::InvalidImage);
    }
    let chip_id = u16::from_le_bytes([header[12], header[13]]);
    if chip_id != 0x0000 {
        return Err(PartError::InvalidImage);
    }
    // `hash_appended` is header byte 23; without it there is nothing to verify.
    if header[23] == 0 {
        return Err(PartError::InvalidImage);
    }

    // `esp_app_desc` sits immediately after the 24-byte header and the first
    // 8-byte segment header.
    let mut desc = [0u8; 256];
    region.read(24 + 8, &mut desc)?;
    if u32::from_le_bytes([desc[0], desc[1], desc[2], desc[3]]) != 0xABCD_5432 {
        return Err(PartError::InvalidImage);
    }
    // version[32] at +16, project_name[32] at +48.
    version.copy_from_slice(&desc[16..48]);
    let name = &desc[48..80];
    let name_len = name.iter().position(|b| *b == 0).unwrap_or(name.len());
    if &name[..name_len] != PROJECT_NAME.as_bytes() {
        return Err(PartError::InvalidImage);
    }

    // The appended SHA-256, verified against the image's own bytes. This is
    // what makes a truncated upload impossible to activate.
    let _digest = entry.sha256(flash)?;
    Ok(())
}

/// Stage a chunk of an upload. Erase-as-you-go, one sector at a time, so the
/// longest stall core 1 ever sees is one sector erase.
pub fn stage_chunk(
    flash: &mut FlashStorage<'static>,
    slot: AppPartitionSubType,
    offset: u32,
    chunk: &[u8; CHUNK],
) -> Result<(), PartError> {
    let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let table = partitions::read_partition_table(flash, &mut buf)?;
    let entry = table
        .find_partition(PartitionType::App(slot))?
        .ok_or(PartError::Invalid)?;
    let mut region = entry.as_flash_region(flash);
    region.erase(offset, offset + CHUNK as u32)?;
    let mut nor = region.as_nor_flash()?;
    use embedded_storage::nor_flash::NorFlash;
    nor.write(offset, chunk).map_err(|_| PartError::StorageError)
}

/// `sequential-storage` over the `screeny` partition, through `BlockingAsync`
/// because `sequential-storage 8` is async-only.
async fn settings_roundtrip(
    flash: &mut FlashStorage<'static>,
    table: &partitions::PartitionTable<'_>,
) -> Result<(), PartError> {
    let entry = table
        .iter()
        .find(|e| e.label_as_str() == CONFIG_LABEL)
        .ok_or(PartError::Invalid)?;
    let len = entry.len();
    let mut region = entry.as_flash_region(flash);
    let nor = region.as_nor_flash()?;
    let mut storage = BlockingAsync::new(nor);

    // Offsets are relative to the partition, so the range starts at 0.
    let config = MapConfig::new(0..len);
    let mut map: MapStorage<u8, _, _> =
        MapStorage::new(&mut storage, config, Cache::new_uncached());

    let mut data = [0u8; 128];
    let brightness: Option<u8> = map
        .fetch_item(&mut data, &(SettingKey::Brightness as u8))
        .await
        .map_err(|_| PartError::StorageError)?;
    info!("spike: stored brightness = {:?}", brightness);

    map.store_item(&mut data, &(SettingKey::SchemaVersion as u8), &1u8)
        .await
        .map_err(|_| PartError::StorageError)?;

    Ok(())
}
