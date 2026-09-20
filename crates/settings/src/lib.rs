//! The device's persisted settings.
//!
//! One crate knows how a setting is laid out in flash: the key set, the value
//! encoding, the schema version, the load/save policy and the write debounce
//! all live here and nowhere else. The firmware hands [`Store`] the real
//! `screeny` partition; these tests hand it `sequential-storage`'s RAM
//! `mock_flash`, so the whole of the behaviour below is exercised on the host
//! by a plain `cargo test`.
//!
//! The precedent is `screeny-receiver`: `no_std`, no allocation, no clock and
//! no I/O of its own.
//!
//! * **No allocation, and no buffer of its own.** Every [`Store`] call takes
//!   the caller's scratch buffer. Research 006 section 2 measured what happens
//!   when buffers are held across an `await` inside an embassy task: the
//!   task's future is a `static`, and 11 KB of `.bss` appeared. Use
//!   [`Scratch`], which is the right size *and* word-aligned -
//!   `sequential-storage` passes the caller's buffer straight to
//!   `NorFlash::write`, and the ESP32 flash backend requires alignment.
//! * **No clock.** [`Debounce`] takes `now_ms` as an argument, so card 063's
//!   "a 60 s slider sweep must not be 1800 flash writes" is a host test that
//!   runs in microseconds.
//! * **Load never fails.** [`Store::load`] returns a usable [`Settings`] and a
//!   [`LoadReport`] whatever it finds: a blank partition, a schema version
//!   from the future, a flash that errors, a page of random bytes. It never
//!   panics, and it never erases anything.
//!
//! # Shape
//!
//! ```ignore
//! let mut scratch = Scratch::new();
//! let mut store = Store::new(flash, 0..PARTITION_LEN)?;
//!
//! let (settings, report) = store.load(scratch.as_mut_slice()).await;
//! if !report.is_clean() { log(report); }      // but carry on regardless
//!
//! // a slider, sixty times a second:
//! debounce.note_change(Field::Brightness, now_ms);
//! // ...and the store task, 3 s later:
//! if let Some(Field::Brightness) = debounce.due(now_ms) {
//!     store.save_brightness(scratch.as_mut_slice(), live_brightness).await?;
//! }
//! ```
//!
//! The flash layout contract, for whoever adds the next key, is
//! `crates/settings/README.md`.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod debounce;
pub mod store;
pub mod value;

pub use debounce::{Debounce, Field, QUIET_MS};
pub use store::{
    key, Fields, LoadReport, MapConfigError, SchemaState, Scratch, Store, StoreError, Write,
    MAX_ITEM_LEN, SCHEMA_VERSION, SCRATCH_LEN, SCRATCH_MIN,
};
pub use value::{Name, Psk, SettingError, Settings, Ssid, Wifi, DEFAULT_BRIGHTNESS};

// The limits are `screeny-proto`'s, not ours: the store cannot hold something
// the wire could not have carried.
pub use screeny_proto::control::{IdleMode, MAX_NAME_LEN, MAX_PSK_LEN, MAX_SSID_LEN};

const _: () = assert!(MAX_SSID_LEN == 32);
const _: () = assert!(MAX_PSK_LEN == 64);
const _: () = assert!(MAX_NAME_LEN == 32);
const _: () = assert!(SCRATCH_MIN >= MAX_ITEM_LEN.next_multiple_of(4));
const _: () = assert!(SCRATCH_LEN >= SCRATCH_MIN);
