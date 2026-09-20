//! A paper `otadata` and a paper bootloader, so "what boots if the power goes
//! *here*" can be run rather than argued about.
//!
//! Nothing in this module reaches the firmware: it is behind the `model`
//! feature, which the firmware turns off. It exists because card 241's safety
//! claim is a claim about **interruptions** - about a dozen instants at which
//! losing power must still leave a device that boots something - and the only
//! honest way to check a dozen of those is to write the bootloader down and
//! cut the power in a loop.
//!
//! Two implementations are modelled, and it matters which is which:
//!
//! * [`OtaData::select_app`] and [`OtaData::set_state`] are
//!   `esp-bootloader-esp-idf 0.6.0`'s `Ota::set_current_app_partition` and
//!   `set_current_ota_state` (`src/ota.rs` lines 269-337 and 355-383): the
//!   sequence arithmetic, *which* of the two entries gets written, and the fact
//!   that neither call looks at the other entry's state.
//! * [`OtaData::boot`] is ESP-IDF `release/v6.1`'s
//!   `bootloader_utility_get_selected_boot_partition` plus
//!   `bootloader_common_ota_select_valid` / `_invalid`
//!   (`components/bootloader_support/src/bootloader_utility.c` lines 445-451 and
//!   510-515; `bootloader_common_loader.c` lines 73-93), with
//!   `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y`, which is what
//!   `firmware/bootloader/esp32-rollback-bootloader.bin` is built with.
//!
//! What is *not* modelled, deliberately: flash encryption, anti-rollback
//! secure versions, a `factory` partition and a `test` partition. This device's
//! partition table has none of them (`firmware/partitions.csv`), and modelling
//! branches that cannot be reached would be modelling fiction.

use crate::{classify, Boot, FwSlot, FwState, RevertReason};

/// The two slots, as `otadata` and the bootloader count them.
pub const SLOTS: usize = 2;

/// An `ota_seq` of all-ones: what an erased sector reads as.
const UNINITIALIZED: u32 = u32::MAX;

/// How a write that did not finish left the 32 bytes it was writing.
///
/// Both are reachable and both are safe, for different reasons, so both are
/// modelled and the tests run every cut twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Damage {
    /// The sector erase completed and the write did not: all-ones, which reads
    /// back as `ota_seq == 0xFFFFFFFF` and is rejected by
    /// `bootloader_common_ota_select_invalid`'s first test.
    Erased,
    /// Some of the 32 bytes landed and the CRC - which is the **last** four -
    /// did not, so `s->crc == bootloader_common_ota_select_crc(s)` fails and
    /// `bootloader_common_ota_select_valid` is false.
    Torn,
}

/// One of `otadata`'s two 32-byte selection entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// Never written, or erased: `ota_seq` all-ones, state `Undefined`.
    Erased,
    /// A write caught half way; the CRC does not match the sequence.
    Torn,
    /// A complete entry.
    Live {
        /// `ota_seq`. The app it selects is `(ota_seq - 1) % 2`.
        seq: u32,
        /// `ota_state`.
        state: FwState,
    },
}

impl Entry {
    fn seq(self) -> u32 {
        match self {
            Entry::Live { seq, .. } => seq,
            // A torn entry's sequence bytes may well have landed, but nothing
            // ever reads them: every path tests validity first.
            Entry::Erased | Entry::Torn => UNINITIALIZED,
        }
    }

    fn state(self) -> FwState {
        match self {
            Entry::Live { state, .. } => state,
            Entry::Erased | Entry::Torn => FwState::Undefined,
        }
    }

    /// `bootloader_common_ota_select_invalid`: all-ones, `INVALID` or `ABORTED`.
    fn is_invalid(self) -> bool {
        match self {
            Entry::Erased => true,
            Entry::Torn => false,
            Entry::Live { seq, state } => {
                seq == UNINITIALIZED
                    || matches!(state, FwState::Invalid | FwState::Aborted)
            }
        }
    }

    /// `bootloader_common_ota_select_valid`: not invalid, and the CRC matches.
    fn is_valid(self) -> bool {
        !self.is_invalid() && !matches!(self, Entry::Torn)
    }

    /// Whether `bootloader_utility` would call this "initial contents": an
    /// all-ones sequence or a CRC that does not match.
    fn is_initial(self) -> bool {
        match self {
            Entry::Erased | Entry::Torn => true,
            Entry::Live { seq, .. } => seq == UNINITIALIZED,
        }
    }

    fn damaged(d: Damage) -> Entry {
        match d {
            Damage::Erased => Entry::Erased,
            Damage::Torn => Entry::Torn,
        }
    }
}

/// What a call into `esp-bootloader-esp-idf` could not do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaError {
    /// `OtaSelectEntry::read` refused an entry - a bad CRC or a state this
    /// crate's enum does not know. Both of the crate's calls read *both*
    /// entries first, so one damaged entry fails them both.
    Unreadable,
    /// No slot is selected at all: `set_current_ota_state` on a completely
    /// erased `otadata`.
    NoSelection,
}

/// `otadata`: two entries, in two different 4 KB sectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtaData {
    /// Entry 0 at +0x0000 and entry 1 at +0x1000. **Different sectors**, which
    /// is what makes a write to one of them unable to damage the other, and so
    /// what makes the whole design atomic.
    pub entries: [Entry; SLOTS],
}

impl Default for OtaData {
    fn default() -> Self {
        Self::erased()
    }
}

impl OtaData {
    /// What `tools/fw-run.sh`'s `--erase-data-parts ota` leaves behind.
    #[must_use]
    pub fn erased() -> Self {
        OtaData {
            entries: [Entry::Erased; SLOTS],
        }
    }

    fn seqs(&self) -> (u32, u32) {
        (self.entries[0].seq(), self.entries[1].seq())
    }

    fn readable(&self) -> Result<(), OtaError> {
        if self.entries.iter().any(|e| matches!(e, Entry::Torn)) {
            Err(OtaError::Unreadable)
        } else {
            Ok(())
        }
    }

    /// `Ota::current_slot`: which of the two *entries* the crate considers
    /// current. Note that it is decided by the sequence numbers alone.
    fn current_entry(&self) -> usize {
        let (s0, s1) = self.seqs();
        if s0 == UNINITIALIZED && s1 == UNINITIALIZED {
            0 // OtaDataSlot::None, whose offset is entry 0's
        } else if s0 == UNINITIALIZED {
            1
        } else if s1 == UNINITIALIZED || s0 > s1 {
            0
        } else {
            1
        }
    }

    /// `Ota::current_app_partition`: the app slot `otadata` selects, or `None`
    /// for "factory", which on this partition table means "nothing selected".
    ///
    /// **This is the call that ignores the states.** After a rollback it still
    /// names the slot that was rolled back from, which is why the firmware asks
    /// the MMU which slot is running and compares the two.
    ///
    /// # Errors
    /// [`OtaError::Unreadable`] if either entry is damaged.
    pub fn current_app(&self) -> Result<Option<u8>, OtaError> {
        self.readable()?;
        let (s0, s1) = self.seqs();
        let counter = if s0 == UNINITIALIZED && s1 == UNINITIALIZED {
            return Ok(None);
        } else if s0 == UNINITIALIZED {
            s1 - 1
        } else if s1 == UNINITIALIZED || s0 > s1 {
            s0 - 1
        } else {
            s0.max(s1) - 1
        };
        Ok(Some((counter % SLOTS as u32) as u8))
    }

    /// `Ota::current_ota_state`.
    ///
    /// # Errors
    /// [`OtaError::Unreadable`], or [`OtaError::NoSelection`] when neither
    /// entry has ever been written.
    pub fn current_state(&self) -> Result<FwState, OtaError> {
        self.readable()?;
        let (s0, s1) = self.seqs();
        if s0 == UNINITIALIZED && s1 == UNINITIALIZED {
            return Err(OtaError::NoSelection);
        }
        Ok(self.entries[self.current_entry()].state())
    }

    /// `Ota::set_current_app_partition`: make `app` the one the bootloader
    /// picks, by writing a higher sequence number into **the entry that is not
    /// current**.
    ///
    /// Returns which entry it wrote, or `None` when the slot was already
    /// selected and the crate wrote nothing at all - which is not a corner
    /// case: it is what happens on the next activation after a rollback.
    ///
    /// `cut` models losing power inside the write.
    ///
    /// # Errors
    /// [`OtaError::Unreadable`].
    pub fn select_app(&mut self, app: u8, cut: Option<Damage>) -> Result<Option<usize>, OtaError> {
        self.readable()?;
        let current = self.current_app()?;
        if current == Some(app) {
            return Ok(None);
        }
        let inc = match current {
            None => (u32::from(app) + 1) % SLOTS as u32,
            Some(c) => (u32::from(app) + SLOTS as u32 - u32::from(c)) % SLOTS as u32,
        };
        let target = (self.current_entry() + 1) % SLOTS;
        let (s0, s1) = self.seqs();
        let new_seq = if s0 == UNINITIALIZED && s1 == UNINITIALIZED {
            inc
        } else if s0 == UNINITIALIZED {
            s1 + inc
        } else if s1 == UNINITIALIZED {
            s0 + inc
        } else {
            s0.max(s1) + inc
        };
        self.entries[target] = match cut {
            Some(d) => Entry::damaged(d),
            // The crate reads the target entry, changes `ota_seq` and `crc`,
            // and writes all 32 bytes back: the **state is carried over**,
            // which is interruption 5b's whole story.
            None => Entry::Live {
                seq: new_seq,
                state: self.entries[target].state(),
            },
        };
        Ok(Some(target))
    }

    /// `Ota::set_current_ota_state`: write a state into the **current** entry.
    ///
    /// # Errors
    /// [`OtaError::Unreadable`] or [`OtaError::NoSelection`].
    pub fn set_state(&mut self, state: FwState, cut: Option<Damage>) -> Result<usize, OtaError> {
        self.readable()?;
        let (s0, s1) = self.seqs();
        if s0 == UNINITIALIZED && s1 == UNINITIALIZED {
            return Err(OtaError::NoSelection);
        }
        let i = self.current_entry();
        self.entries[i] = match cut {
            Some(d) => Entry::damaged(d),
            None => Entry::Live {
                seq: self.entries[i].seq(),
                state,
            },
        };
        Ok(i)
    }

    /// ESP-IDF v6.1's `bootloader_utility_get_selected_boot_partition`, with
    /// `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y`.
    ///
    /// Returns the app slot it selected, or `None` for "try all partitions".
    /// **It writes `otadata`**, twice over: any `PENDING_VERIFY` becomes
    /// `ABORTED` before anything is selected, and a selected `NEW` becomes
    /// `PENDING_VERIFY` after.
    fn select_boot(&mut self) -> Option<u8> {
        // Lines 445-451: unconditional, on every reset, whatever the reason.
        for e in &mut self.entries {
            if let Entry::Live {
                seq,
                state: FwState::PendingVerify,
            } = *e
            {
                *e = Entry::Live {
                    seq,
                    state: FwState::Aborted,
                };
            }
        }

        if self.entries[0].is_invalid() && self.entries[1].is_invalid() {
            // "No factory image, trying OTA 0."
            return Some(0);
        }

        // `bootloader_common_get_active_otadata`: the highest sequence among
        // the *valid* entries, or nothing.
        let active = (0..SLOTS)
            .filter(|i| self.entries[*i].is_valid())
            .max_by_key(|i| self.entries[*i].seq())?;
        let seq = self.entries[active].seq();
        let boot_index = ((seq - 1) % SLOTS as u32) as u8;

        // Lines 510-515.
        if self.entries[active].state() == FwState::New {
            self.entries[active] = Entry::Live {
                seq,
                state: FwState::PendingVerify,
            };
        }
        Some(boot_index)
    }

    /// Would `bootloader_utility` decide `otadata` has never been written, and
    /// write a fresh `ota_seq` of its own?
    fn is_initial(&self) -> bool {
        self.entries.iter().all(|e| e.is_initial())
    }
}

// ---------------------------------------------------------------------------
// The device
// ---------------------------------------------------------------------------

/// What is in an app slot, as far as booting it is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Image {
    /// Boots, joins, serves, swaps: confirms itself at 60 s.
    Good,
    /// Boots and runs for ever without ever meeting the health criterion. The
    /// app-side deadline is the only thing that catches it.
    NeverHealthy,
    /// Boots and panics before the confirm deadline. The panic handler resets
    /// the chip; the bootloader is what catches it.
    Panics,
    /// Boots and then hangs with nothing running. Only the RTC watchdog catches
    /// it, and that is a reset like any other.
    Hangs,
    /// Structurally broken: the bootloader's own image verification refuses it
    /// and it never runs at all.
    Broken,
    /// An erased slot.
    Absent,
}

impl Image {
    fn is_bootable(self) -> bool {
        !matches!(self, Image::Broken | Image::Absent)
    }
}

/// What the image that booted did with its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ran {
    /// It was not on trial and had nothing to prove.
    Settled,
    /// It was on trial and marked itself `VALID`.
    Confirmed,
    /// It was on trial, never became healthy, and marked itself `INVALID`
    /// before resetting.
    RevertedAtDeadline,
    /// It reset without marking anything - a panic, or the watchdog after a
    /// hang.
    ResetWithoutConfirming,
    /// Nothing bootable was found.
    Nothing,
}

/// One device: two app slots, one `otadata`, one running image.
#[derive(Debug, Clone, Copy)]
pub struct Device {
    /// The two app slots' contents.
    pub slots: [Image; SLOTS],
    /// `otadata`.
    pub otadata: OtaData,
    /// Which slot is running, once [`Device::power_on`] has been called.
    pub running: Option<u8>,
    /// What [`classify`] made of this boot.
    pub boot: Boot,
}

impl Device {
    /// A device as `tools/fw-run.sh` leaves it: a good image in `ota_0`, an
    /// erased `ota_1`, and an erased `otadata`.
    #[must_use]
    pub fn freshly_flashed() -> Self {
        Device {
            slots: [Image::Good, Image::Absent],
            otadata: OtaData::erased(),
            running: None,
            boot: Boot::Unknown,
        }
    }

    /// Run the bootloader, then work out what kind of boot this is.
    ///
    /// Every reset goes through here, whatever caused it: that is the point -
    /// the abort loop does not care why.
    pub fn power_on(&mut self) {
        let initial = self.otadata.is_initial();
        let selected = self.otadata.select_boot();
        // `bootloader_utility_load_boot_image`: try the selected slot, then the
        // others, and take the first that verifies.
        let order: [u8; SLOTS] = match selected {
            Some(0) | None => [0, 1],
            _ => [1, 0],
        };
        self.running = order.into_iter().find(|i| self.slots[*i as usize].is_bootable());

        if initial && self.running.is_some() {
            // `set_actual_ota_seq`: with no factory partition and no usable
            // otadata, the bootloader writes a fresh, VALID entry 0 - which is
            // why a serial-flashed device reports `fw_state valid` and never
            // goes anywhere near the trial machinery.
            self.otadata.entries[0] = Entry::Live {
                seq: 1,
                state: FwState::Valid,
            };
        }

        self.boot = match self.running {
            None => Boot::Unknown,
            Some(run) => classify(
                slot(run),
                self.otadata
                    .current_app()
                    .ok()
                    .flatten()
                    .map_or(FwSlot::Unknown, slot),
                self.otadata.current_state().unwrap_or(FwState::Undefined),
            ),
        };
    }

    /// Let the running image do whatever its kind does, including the trial
    /// machinery the firmware runs.
    ///
    /// Returns what it did; a [`Ran::RevertedAtDeadline`] or
    /// [`Ran::ResetWithoutConfirming`] is followed by a reset, which the caller
    /// models by calling [`Device::power_on`] again.
    pub fn run(&mut self) -> Ran {
        let Some(run) = self.running else {
            return Ran::Nothing;
        };
        // Card 241: an `Unproven` boot is promoted before anything else, so
        // that the *next* reset - whatever causes it - is one the bootloader
        // will roll back.
        if self.boot == Boot::Unproven {
            let _ = self.otadata.set_state(FwState::PendingVerify, None);
            self.boot = Boot::Trial;
        }
        if self.boot != Boot::Trial {
            return Ran::Settled;
        }
        match self.slots[run as usize] {
            Image::Good => {
                let _ = self.otadata.set_state(FwState::Valid, None);
                Ran::Confirmed
            }
            Image::NeverHealthy => {
                let _ = self.otadata.set_state(FwState::Invalid, None);
                Ran::RevertedAtDeadline
            }
            // A panic resets the chip and a hang is resolved by the RTC
            // watchdog, which also resets the chip. Neither writes `otadata`:
            // the bootloader's abort loop is the whole mechanism.
            Image::Panics | Image::Hangs => Ran::ResetWithoutConfirming,
            Image::Broken | Image::Absent => Ran::Nothing,
        }
    }

    /// Boot, and let the image run, until the device settles or `limit` boots
    /// have happened. Returns the number of boots it took.
    ///
    /// A device that keeps resetting is a bug, and `limit` is how the tests say
    /// so instead of hanging.
    pub fn settle(&mut self, limit: usize) -> usize {
        for n in 1..=limit {
            self.power_on();
            match self.run() {
                Ran::Settled | Ran::Confirmed | Ran::Nothing => return n,
                Ran::RevertedAtDeadline | Ran::ResetWithoutConfirming => {}
            }
        }
        limit + 1
    }

    /// The slot an upload would be staged into: the one that is not running.
    ///
    /// The firmware's `store::InactiveSlot`, which is decided from the MMU and
    /// never from `otadata`.
    #[must_use]
    pub fn inactive(&self) -> Option<u8> {
        self.running.map(|r| 1 - r)
    }
}

/// Where an activation can be interrupted.
///
/// The names are card 241's interruption table, and the tests walk every one of
/// them for both kinds of [`Damage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cut {
    /// Nothing is interrupted: the whole activation lands and the device resets.
    None,
    /// Row 5a: inside the sequence write.
    DuringSelect(Damage),
    /// Row 5b: after the sequence write, before the state write.
    BeforeMarkNew,
    /// Row 5b again, with the state write itself caught half way.
    DuringMarkNew(Damage),
    /// Row 6: both writes landed, the reset did not happen.
    BeforeReset,
}

/// Stage an image into the inactive slot and activate it, cutting the power at
/// `cut`.
///
/// Returns `true` if the activation ran to its reset. Staging itself is card
/// 240's and is modelled as "the image is now in the inactive slot", because
/// nothing about staging can reach `otadata`.
///
/// # Panics
/// If the device is not running anything, which is a broken test rather than a
/// state a device can be in.
pub fn upload_and_activate(dev: &mut Device, image: Image, cut: Cut) -> bool {
    let target = dev.inactive().expect("a running device has an inactive slot");
    dev.slots[target as usize] = image;

    let select_cut = match cut {
        Cut::DuringSelect(d) => Some(d),
        _ => None,
    };
    let _ = dev.otadata.select_app(target, select_cut);
    if select_cut.is_some() || cut == Cut::BeforeMarkNew {
        return false;
    }
    let mark_cut = match cut {
        Cut::DuringMarkNew(d) => Some(d),
        _ => None,
    };
    let _ = dev.otadata.set_state(FwState::New, mark_cut);
    if mark_cut.is_some() || cut == Cut::BeforeReset {
        return false;
    }
    true
}

fn slot(i: u8) -> FwSlot {
    match i {
        0 => FwSlot::Ota0,
        1 => FwSlot::Ota1,
        _ => FwSlot::Unknown,
    }
}

/// Every [`Cut`] there is, for a test that wants to walk them.
#[must_use]
pub fn every_cut() -> [Cut; 7] {
    [
        Cut::None,
        Cut::DuringSelect(Damage::Erased),
        Cut::DuringSelect(Damage::Torn),
        Cut::BeforeMarkNew,
        Cut::DuringMarkNew(Damage::Erased),
        Cut::DuringMarkNew(Damage::Torn),
        Cut::BeforeReset,
    ]
}

/// The revert reason a device reports after a rolled-back update, if it is
/// reporting one at all.
#[must_use]
pub fn reported_reason(dev: &Device) -> Option<RevertReason> {
    dev.boot.revert_reason()
}
