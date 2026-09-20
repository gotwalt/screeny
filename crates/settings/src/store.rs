//! The flash side: the key set, the schema version, and the load/save policy.
//!
//! See the crate docs and `crates/settings/README.md` for the layout contract.

use core::ops::Range;

use embedded_storage_async::nor_flash::NorFlash;
use heapless::Vec;
use screeny_proto::control::{IdleMode, MAX_PSK_LEN, MAX_SSID_LEN};
use sequential_storage::cache::{Cache, Uncached};
use sequential_storage::map::{MapConfig, MapStorage};
use sequential_storage::Error as SsError;

pub use sequential_storage::map::MapConfigError;

use crate::value::{Name, Psk, SettingError, Settings, Ssid, Wifi};

/// The schema version this build writes and understands.
///
/// Bump it only when an existing key changes meaning or encoding. Adding a new
/// key does **not** need a bump: an old build ignores keys it does not fetch,
/// and a new build treats a missing key as "not set" (see [`key`]).
pub const SCHEMA_VERSION: u8 = 1;

/// The key byte of every item in the map. `sequential-storage` implements its
/// `Key` trait for `u8`, so these are the keys verbatim.
///
/// Numbers are permanent. A retired key is never reused; the next key is 6.
pub mod key {
    /// [`super::SCHEMA_VERSION`], a `u8`. Written first, read first.
    pub const SCHEMA_VERSION: u8 = 0;
    /// The SSID, 0 to 32 bytes. **Zero bytes means "no credentials"**, which
    /// is how `clear_wifi` works without needing a `MultiwriteNorFlash`.
    pub const WIFI_SSID: u8 = 1;
    /// The PSK, 0 to 64 bytes. Zero bytes is an open network.
    pub const WIFI_PSK: u8 = 2;
    /// The friendly name, 0 to 32 UTF-8 bytes. Zero bytes means `screeny-<id>`.
    pub const NAME: u8 = 3;
    /// Panel brightness, a `u8`.
    pub const BRIGHTNESS: u8 = 4;
    /// Idle mode, a `u8`; interpret with `IdleMode::from_u8`.
    pub const IDLE_MODE: u8 = 5;
}

/// The longest item this schema can produce: one key byte plus the longest
/// value, which is the 64-byte PSK.
pub const MAX_ITEM_LEN: usize = 1 + MAX_PSK_LEN;

/// The smallest scratch buffer [`Store`] will accept.
///
/// `sequential-storage` needs the longest serialized key + value rounded up to
/// the flash word size: 65 bytes rounded up to a 4-byte ESP32 word is 68. 72
/// is that with one word of slack, and it is the number every `Store` method
/// checks before it touches flash.
pub const SCRATCH_MIN: usize = 72;

/// The scratch buffer size the firmware should actually supply: [`SCRATCH_MIN`]
/// rounded up, with room for a key of up to ~120 bytes to be added later
/// without a second look at this constant. Use [`Scratch`], which is also
/// aligned.
pub const SCRATCH_LEN: usize = 128;

/// A correctly sized and **4-byte aligned** scratch buffer.
///
/// Alignment is not a nicety. `sequential-storage` hands the caller's buffer
/// straight to `NorFlash::write` (`item.rs`, `Item::write_new`), and
/// `esp-storage`'s ESP32 flash backend requires a word-aligned buffer; the
/// `mock_flash` used by this crate's tests panics on an unaligned one when
/// `alignment_check` is on, which is how this stays honest. A plain
/// `[0u8; 128]` on the stack is *not* guaranteed to be aligned, so the
/// firmware should own one of these instead.
///
/// It is deliberately a value the caller keeps, not a `static` and not
/// something inside a future: research 006 section 2 measured 11 KB of `.bss`
/// appearing the moment buffers were held across an `await` inside an embassy
/// task.
#[derive(Clone, Copy)]
#[repr(align(4))]
pub struct Scratch([u8; SCRATCH_LEN]);

impl Scratch {
    /// A fresh buffer. `const` so it can live in a `static mut`-free
    /// initialiser.
    #[must_use]
    pub const fn new() -> Self {
        Self([0; SCRATCH_LEN])
    }

    /// The buffer to hand to [`Store`].
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl AsMut<[u8]> for Scratch {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl Default for Scratch {
    fn default() -> Self {
        Self::new()
    }
}

/// Everything that can go wrong talking to the store.
///
/// Deliberately not generic over the flash's own error type: the firmware maps
/// every one of these to `ERR_STORAGE` (spec section 6.5), and a non-generic,
/// `Copy` enum is what a telemetry field and a log line want. The flash's own
/// error value is dropped at the boundary; [`StoreError::Flash`] is as much as
/// survives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    /// The flash itself reported an error (or lost power mid-write, in the
    /// tests).
    Flash,
    /// The store's contents are inconsistent and a repair pass did not fix it.
    /// The caller may offer a factory reset; this crate never erases by itself.
    Corrupted,
    /// The partition has no room left for another item.
    Full,
    /// The scratch buffer is too small. Carries the number of bytes needed,
    /// which is always [`SCRATCH_MIN`].
    Scratch(usize),
    /// The value offered is not one this schema can hold. Nothing was written.
    Invalid(SettingError),
    /// A stored item could not be deserialized, or `sequential-storage`
    /// reported a bug in itself rather than panicking.
    Internal,
}

impl<E> From<SsError<E>> for StoreError {
    fn from(e: SsError<E>) -> Self {
        match e {
            SsError::Storage { .. } => StoreError::Flash,
            SsError::Corrupted { .. } => StoreError::Corrupted,
            SsError::FullStorage | SsError::ItemTooBig => StoreError::Full,
            SsError::BufferTooSmall(n) => StoreError::Scratch(n),
            SsError::BufferTooBig | SsError::SerializationError(_) | SsError::LogicBug { .. } => {
                StoreError::Internal
            }
            // `Error` is `#[non_exhaustive]`.
            _ => StoreError::Internal,
        }
    }
}

impl From<SettingError> for StoreError {
    fn from(e: SettingError) -> Self {
        StoreError::Invalid(e)
    }
}

/// What a save actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    /// Flash already held this value; nothing was written. This is the common
    /// case behind a debounced slider and the reason flash survives one.
    Skipped,
    /// The value was written.
    Committed,
}

/// What the schema version item said at load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchemaState {
    /// No version item at all: a blank (all-`0xFF`) or freshly erased
    /// partition. Not an error. This is the default because it is also what a
    /// `LoadReport` says before anything has been read.
    #[default]
    Blank,
    /// The version this build writes.
    Current,
    /// Some other version. Everything else in the map is ignored and the
    /// defaults are used; the next save rewrites the partition at
    /// [`SCHEMA_VERSION`].
    Unknown(u8),
    /// The version item could not be read at all. See
    /// [`LoadReport::error`].
    Unreadable,
}

/// Which settings fell back to their default at load. A bitset so a
/// [`LoadReport`] stays `Copy` and fits in a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Fields(u8);

impl Fields {
    /// Nothing.
    pub const NONE: Self = Self(0);
    /// The Wi-Fi credentials.
    pub const WIFI: Self = Self(1 << 0);
    /// The friendly name.
    pub const NAME: Self = Self(1 << 1);
    /// The brightness.
    pub const BRIGHTNESS: Self = Self(1 << 2);
    /// The idle mode.
    pub const IDLE_MODE: Self = Self(1 << 3);
    /// All four.
    pub const ALL: Self = Self(0b1111);

    /// True when `other`'s bits are all set here.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// True when no field fell back.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The raw bits, for a log line.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// What [`Store::load`] found. Load itself never fails: it always returns a
/// usable [`Settings`], and this says how much of it came out of flash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LoadReport {
    /// What the schema version item said.
    pub schema: SchemaState,
    /// The settings that are at their default because the stored value was
    /// missing or unusable.
    pub fallback: Fields,
    /// The first store error hit, if any. A blank partition is not an error.
    pub error: Option<StoreError>,
}

impl LoadReport {
    /// True when every setting came out of flash and nothing went wrong.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        matches!(self.schema, SchemaState::Current)
            && self.fallback.is_empty()
            && self.error.is_none()
    }

    /// True when the partition simply has nothing in it yet: a new device, or
    /// one just factory-reset. Not a fault.
    #[must_use]
    pub const fn is_blank(&self) -> bool {
        matches!(self.schema, SchemaState::Blank) && self.error.is_none()
    }

    fn note(&mut self, field: Fields, err: Option<StoreError>) {
        self.fallback = self.fallback.with(field);
        if self.error.is_none() {
            self.error = err;
        }
    }
}

type NoCache = Cache<Uncached, Uncached, Uncached, u8>;

/// The settings store over one NOR flash region.
///
/// Generic over any `embedded-storage-async` [`NorFlash`]: the firmware hands
/// it the `screeny` partition wrapped in `BlockingAsync`, the tests hand it
/// `sequential-storage`'s `mock_flash`. The crate does no I/O of its own, keeps
/// no clock, allocates nothing, and holds no buffer: every call takes the
/// caller's scratch buffer (see [`Scratch`]).
///
/// The cache is deliberately [`Cache::new_uncached`]. A cache would trade RAM
/// and a correctness hazard (it must match flash exactly or "undesirable things
/// will happen") for speed the device does not need: settings are read once at
/// boot and written a few times a minute at worst.
pub struct Store<F: NorFlash> {
    map: MapStorage<u8, F, NoCache>,
}

impl<F: NorFlash> Store<F> {
    /// Open the store over `flash_range`.
    ///
    /// The range is **relative to the flash object**, which for the firmware
    /// means partition-relative: `esp-bootloader-esp-idf`'s `NorFlashRegion`
    /// adds the partition offset itself, so this is `0..len`. Both ends must
    /// sit on an erase boundary and the range must cover at least two pages.
    ///
    /// # Errors
    /// [`MapConfigError`] if the range is not usable. This is the fallible
    /// `MapConfig::try_new`; the infallible `MapConfig::new` panics, and a
    /// panic is not available to us.
    pub fn new(flash: F, flash_range: Range<u32>) -> Result<Self, MapConfigError> {
        let config = MapConfig::try_new(flash_range)?;
        Ok(Self {
            map: MapStorage::new(flash, config, Cache::new_uncached()),
        })
    }

    /// The flash back, for a caller that needs it (and for tests that want the
    /// mock's counters).
    pub fn flash(&mut self) -> &mut F {
        self.map.flash()
    }

    /// Read every setting.
    ///
    /// **This never fails and never panics.** Any error, any corruption, any
    /// value this build cannot interpret becomes "use the default for that
    /// setting" plus a note in the [`LoadReport`]. Nothing is erased: a
    /// factory reset is [`Store::erase_all`], and it is the caller's decision.
    ///
    /// A blank partition is the expected state of a new device and reports
    /// [`SchemaState::Blank`] with no error.
    pub async fn load(&mut self, scratch: &mut [u8]) -> (Settings, LoadReport) {
        let mut out = Settings::default();
        let mut report = LoadReport::default();

        if scratch.len() < SCRATCH_MIN {
            report.schema = SchemaState::Unreadable;
            report.note(Fields::ALL, Some(StoreError::Scratch(SCRATCH_MIN)));
            return (out, report);
        }

        match self.fetch_u8(scratch, key::SCHEMA_VERSION).await {
            Err(e) => {
                report.schema = SchemaState::Unreadable;
                report.note(Fields::ALL, Some(e));
                return (out, report);
            }
            Ok(None) => {
                report.schema = SchemaState::Blank;
                report.note(Fields::ALL, None);
                return (out, report);
            }
            Ok(Some(v)) if v != SCHEMA_VERSION => {
                report.schema = SchemaState::Unknown(v);
                report.note(Fields::ALL, None);
                return (out, report);
            }
            Ok(Some(_)) => report.schema = SchemaState::Current,
        }

        // Wi-Fi. An absent or zero-length SSID means "no credentials"; the PSK
        // is not even consulted in that case.
        match self
            .fetch_bytes::<MAX_SSID_LEN>(scratch, key::WIFI_SSID)
            .await
        {
            Err(e) => report.note(Fields::WIFI, Some(e)),
            Ok(None) => report.note(Fields::WIFI, None),
            Ok(Some(raw)) if raw.is_empty() => report.note(Fields::WIFI, None),
            Ok(Some(raw)) => match Ssid::new(&raw) {
                Err(e) => report.note(Fields::WIFI, Some(StoreError::Invalid(e))),
                Ok(ssid) => match self
                    .fetch_bytes::<MAX_PSK_LEN>(scratch, key::WIFI_PSK)
                    .await
                {
                    Err(e) => report.note(Fields::WIFI, Some(e)),
                    Ok(psk) => {
                        let bytes = psk.as_deref().unwrap_or(&[]);
                        match Psk::new(bytes) {
                            Err(e) => report.note(Fields::WIFI, Some(StoreError::Invalid(e))),
                            Ok(psk) => out.wifi = Some(Wifi { ssid, psk }),
                        }
                    }
                },
            },
        }

        match self.fetch_bytes::<32>(scratch, key::NAME).await {
            Err(e) => report.note(Fields::NAME, Some(e)),
            Ok(None) => report.note(Fields::NAME, None),
            Ok(Some(raw)) => match Name::from_bytes(&raw) {
                Err(e) => report.note(Fields::NAME, Some(StoreError::Invalid(e))),
                Ok(name) => out.name = name,
            },
        }

        match self.fetch_u8(scratch, key::BRIGHTNESS).await {
            Err(e) => report.note(Fields::BRIGHTNESS, Some(e)),
            Ok(None) => report.note(Fields::BRIGHTNESS, None),
            Ok(Some(level)) => out.brightness = level,
        }

        match self.fetch_u8(scratch, key::IDLE_MODE).await {
            Err(e) => report.note(Fields::IDLE_MODE, Some(e)),
            Ok(None) => report.note(Fields::IDLE_MODE, None),
            Ok(Some(raw)) => match IdleMode::from_u8(raw) {
                // A mode a later firmware wrote and this one does not know.
                None => report.note(Fields::IDLE_MODE, None),
                Some(mode) => out.idle_mode = mode,
            },
        }

        (out, report)
    }

    /// Store a credential pair.
    ///
    /// Written immediately, never debounced: the caller is about to drop the
    /// current association and needs the new credentials to survive that.
    ///
    /// The SSID item is the commit point. The sequence is **clear the SSID,
    /// write the PSK, write the SSID**, so losing power part-way leaves the
    /// device with the old credentials, with none at all (it comes up in the
    /// setup portal), or with the new ones - never with one network's name and
    /// another's key. See `README.md`.
    ///
    /// # Errors
    /// [`StoreError`]. Nothing is written when the value is refused or the
    /// scratch buffer is too small.
    pub async fn save_wifi(
        &mut self,
        scratch: &mut [u8],
        wifi: &Wifi,
    ) -> Result<Write, StoreError> {
        check_scratch(scratch)?;
        let ssid = wifi.ssid.as_bytes();
        let psk = wifi.psk.as_bytes();
        if ssid.is_empty() {
            return Err(StoreError::Invalid(SettingError::SsidEmpty));
        }
        self.prepare(scratch).await?;

        let cur_ssid = self
            .fetch_bytes::<MAX_SSID_LEN>(scratch, key::WIFI_SSID)
            .await?;
        let cur_psk = self
            .fetch_bytes::<MAX_PSK_LEN>(scratch, key::WIFI_PSK)
            .await?;
        if cur_ssid.as_deref() == Some(ssid) && cur_psk.as_deref().unwrap_or(&[]) == psk {
            return Ok(Write::Skipped);
        }

        if cur_ssid.is_some_and(|s| !s.is_empty()) {
            self.store_bytes(scratch, key::WIFI_SSID, &[]).await?;
        }
        self.store_bytes(scratch, key::WIFI_PSK, psk).await?;
        self.store_bytes(scratch, key::WIFI_SSID, ssid).await?;
        Ok(Write::Committed)
    }

    /// Forget the stored credentials.
    ///
    /// Writes a zero-length SSID rather than removing the item:
    /// `sequential-storage`'s `remove_item` needs a `MultiwriteNorFlash`, and
    /// the ESP32's internal flash is not one.
    ///
    /// The old PSK bytes stay physically in flash until the page they are on is
    /// recycled. Spec section 8.4 says the PSK is not treated as a secret, so
    /// this is recorded rather than defended against; [`Store::erase_all`] is
    /// the path that really removes it.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn clear_wifi(&mut self, scratch: &mut [u8]) -> Result<Write, StoreError> {
        check_scratch(scratch)?;
        self.prepare(scratch).await?;

        let cur_ssid = self
            .fetch_bytes::<MAX_SSID_LEN>(scratch, key::WIFI_SSID)
            .await?;
        let cur_psk = self
            .fetch_bytes::<MAX_PSK_LEN>(scratch, key::WIFI_PSK)
            .await?;
        let ssid_clear = cur_ssid.as_deref().unwrap_or(&[]).is_empty();
        let psk_clear = cur_psk.as_deref().unwrap_or(&[]).is_empty();
        if ssid_clear && psk_clear {
            return Ok(Write::Skipped);
        }

        if !ssid_clear {
            self.store_bytes(scratch, key::WIFI_SSID, &[]).await?;
        }
        if !psk_clear {
            self.store_bytes(scratch, key::WIFI_PSK, &[]).await?;
        }
        Ok(Write::Committed)
    }

    /// Store the friendly name. An empty name means `screeny-<id>`.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn save_name(
        &mut self,
        scratch: &mut [u8],
        name: &Name,
    ) -> Result<Write, StoreError> {
        check_scratch(scratch)?;
        self.prepare(scratch).await?;
        let want = name.as_bytes();
        let cur = self.fetch_bytes::<32>(scratch, key::NAME).await?;
        let skip = match cur.as_deref() {
            Some(stored) => stored == want,
            // No item at all is the same state as an empty name.
            None => want.is_empty(),
        };
        if skip {
            return Ok(Write::Skipped);
        }
        self.store_bytes(scratch, key::NAME, want).await?;
        Ok(Write::Committed)
    }

    /// Store the panel brightness.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn save_brightness(
        &mut self,
        scratch: &mut [u8],
        level: u8,
    ) -> Result<Write, StoreError> {
        self.save_u8(scratch, key::BRIGHTNESS, level).await
    }

    /// Store the idle mode.
    ///
    /// # Errors
    /// [`StoreError`].
    pub async fn save_idle_mode(
        &mut self,
        scratch: &mut [u8],
        mode: IdleMode,
    ) -> Result<Write, StoreError> {
        self.save_u8(scratch, key::IDLE_MODE, mode.as_u8()).await
    }

    /// Factory reset: erase the whole region.
    ///
    /// The only path in this crate that erases, and it is never taken on its
    /// own initiative - a caller asks for it, from the button or a control
    /// opcode. Afterwards [`Store::load`] reports [`SchemaState::Blank`].
    ///
    /// # Errors
    /// [`StoreError::Flash`].
    pub async fn erase_all(&mut self) -> Result<(), StoreError> {
        self.map.erase_all().await.map_err(StoreError::from)
    }

    // -- internals ---------------------------------------------------------

    /// Make sure the schema version item is present and current before a write.
    ///
    /// A *foreign* version is the one case that erases: the other items were
    /// written by a build whose encoding this one does not know, so reading
    /// them under [`SCHEMA_VERSION`] would produce nonsense. They are already
    /// being ignored at load; this drops them and starts the partition again at
    /// the current version. That is a migration, not error recovery - the
    /// "never erase on your own" rule is about a *load* that went wrong.
    async fn prepare(&mut self, scratch: &mut [u8]) -> Result<(), StoreError> {
        match self.fetch_u8(scratch, key::SCHEMA_VERSION).await? {
            Some(SCHEMA_VERSION) => Ok(()),
            Some(_) => {
                self.map.erase_all().await?;
                self.store_u8(scratch, key::SCHEMA_VERSION, SCHEMA_VERSION)
                    .await
            }
            None => {
                self.store_u8(scratch, key::SCHEMA_VERSION, SCHEMA_VERSION)
                    .await
            }
        }
    }

    async fn save_u8(&mut self, scratch: &mut [u8], k: u8, v: u8) -> Result<Write, StoreError> {
        check_scratch(scratch)?;
        self.prepare(scratch).await?;
        if self.fetch_u8(scratch, k).await? == Some(v) {
            return Ok(Write::Skipped);
        }
        self.store_u8(scratch, k, v).await?;
        Ok(Write::Committed)
    }

    async fn fetch_u8(&mut self, scratch: &mut [u8], k: u8) -> Result<Option<u8>, StoreError> {
        self.map
            .fetch_item::<u8>(scratch, &k)
            .await
            .map_err(StoreError::from)
    }

    async fn store_u8(&mut self, scratch: &mut [u8], k: u8, v: u8) -> Result<(), StoreError> {
        self.map
            .store_item::<u8>(scratch, &k, &v)
            .await
            .map_err(StoreError::from)
    }

    /// Fetch a byte-string item and copy it out, so the borrow on `scratch`
    /// ends before the next call.
    async fn fetch_bytes<const N: usize>(
        &mut self,
        scratch: &mut [u8],
        k: u8,
    ) -> Result<Option<Vec<u8, N>>, StoreError> {
        let raw = self.map.fetch_item::<&[u8]>(scratch, &k).await?;
        match raw {
            None => Ok(None),
            // Longer than this schema allows: a later firmware, or damage that
            // still passed the CRC. Either way the caller sees "unusable".
            Some(bytes) => Vec::from_slice(bytes)
                .map(Some)
                .map_err(|_| StoreError::Internal),
        }
    }

    async fn store_bytes(&mut self, scratch: &mut [u8], k: u8, v: &[u8]) -> Result<(), StoreError> {
        self.map
            .store_item::<&[u8]>(scratch, &k, &v)
            .await
            .map_err(StoreError::from)
    }
}

fn check_scratch(scratch: &[u8]) -> Result<(), StoreError> {
    if scratch.len() < SCRATCH_MIN {
        Err(StoreError::Scratch(SCRATCH_MIN))
    } else {
        Ok(())
    }
}
