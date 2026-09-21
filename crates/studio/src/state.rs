//! The state store: one small file that says what plays where.
//!
//! The whole point of card 106 is that the server can be restarted - by a
//! `docker stop`, a host reboot or an image update - and come back showing what
//! it was showing, with nobody watching. That property is this file.
//!
//! Three rules, and the tests in `tests/state.rs` are each of them:
//!
//! 1. **Atomic.** A save writes `state.json.tmp`, `fsync`s it, and renames it
//!    over `state.json`. A reader therefore sees the old file or the new one
//!    and never half of either, whatever happens to the power.
//! 2. **Versioned.** [`SCHEMA_VERSION`] is in the file. A version this build
//!    does not understand is moved aside, not parsed and not deleted.
//! 3. **Never a crash loop.** Missing, empty, truncated, corrupt, wrong-typed
//!    or from-the-future: every one of them starts a sane default and says why
//!    once. A state file is never a reason for the server not to run.
//!
//! Writing happens on one thread with a **one-slot** mailbox: newest wins, a
//! save never blocks the caller, and a burst of slider changes costs one write
//! rather than a queue. Nothing here can grow without bound.

use screeny_art::patch::{ParamSpec, PatchDef};
use screeny_art::Output;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// The schema this build writes and is willing to read.
///
/// - **v1** (card 106): `devices`, `players` and the design view's `preview`.
/// - **v2** (card 165) adds the memory, then called `pieces`: one map for the
///   whole studio, keyed by patch id. What each context was playing is merged
///   into it.
/// - **v3** (card 170) **drops `preview`**. There is one engine now - the
///   player for the attached panel - so the design view has no separate state
///   to keep. A player gains `paused` and `speed`, which used to be the
///   preview's, and the file gains `focus`: which player the page is showing.
/// - **v4** (card 150) is a **renaming and nothing else**: a piece is a patch,
///   so `pieces` is `patches` and a player's `piece` is its `patch`; and the
///   word "settings" is freed for what card 151 will mean by it, so a player's
///   `settings` block is its `output`. Every old key is still read - see the
///   `#[serde(alias)]`s below and [`migrate`] - so a v3 file, or a script
///   writing one, loses nothing.
/// - **v5** (card 151) gives a patch **named settings**. Its entry in
///   `patches` keeps what it always did - the working copy - and gains three
///   things: `speed` (which is part of a setting, so it has to be part of the
///   working copy or a switch away and back would lose it), `setting` (the name
///   the working copy was loaded from, empty for Default) and `settings`: name
///   -> `{seed, params, speed}`. **"Modified" is not in the file**: it is the
///   working copy compared with the setting it names, which cannot go stale.
///
/// Older files are migrated, never thrown away, and are copied aside first.
/// See [`migrate`] and [`back_up`].
pub const SCHEMA_VERSION: u32 = 5;
/// The file, inside the state directory.
pub const FILE: &str = "state.json";
/// Where the last unreadable state file is kept. One fixed name: a server that
/// runs for months must not accumulate rubble.
pub const BAD_FILE: &str = "state.bad.json";

/// What the copy of a file about to be migrated is called: `state.v3.json` for
/// a v3 file. One fixed name per schema, for the same reason [`BAD_FILE`] is
/// one fixed name - and the same name a from-the-future file is kept under, so
/// the directory only ever holds one file per version it has seen.
#[must_use]
pub fn backup_name(was: u64) -> String {
    format!("state.v{was}.json")
}

/// How many memories for patches this build has never heard of are kept.
///
/// The studio itself only ever writes a memory for a patch it can play, so it
/// is already bounded by the number of patches in the binary. This is the bound
/// on the other direction: a file edited by hand, or written by a build with
/// patches this one does not have, cannot grow without limit.
pub const MAX_UNKNOWN_PATCHES: usize = 64;

/// How many "this is what I had to correct" sentences are kept for the status
/// route. The log has them all; the dashboard does not need a novel.
const MAX_REPAIRS: usize = 16;

// ------------------------------------ named settings (card 151) ------------

/// The setting every patch has and nobody can change: its `ParamSpec`
/// defaults, speed 1.0 and [`DEFAULT_SEED`].
///
/// **Not stored.** It is synthesised from the patch, so a release that
/// improves a default improves Default for everybody. Reserved as a name in
/// any case: "default", "DEFAULT" and "Default" are all it.
pub const DEFAULT_SETTING: &str = "Default";

/// The seed Default is on.
///
/// A *fixed* number rather than a fresh one, because Default has to be **one
/// picture**: two people on Default, or the same person before and after a
/// restart, must be looking at the same thing. No patch declares a seed of its
/// own today, so this is the studio's own starting seed
/// ([`StoredPlayer::default`]); a patch that later has an opinion can carry one
/// in its `PatchDef` and this stays the answer for the rest.
pub const DEFAULT_SEED: u32 = 1;

/// Named settings a patch may have. The file is written whole, so this is what
/// keeps it small enough for that to stay true.
pub const MAX_SETTINGS: usize = 64;

/// Characters a name may have, after trimming.
pub const MAX_NAME_CHARS: usize = 40;

/// One named setting: everything about a patch that a person tuned.
///
/// The seed is in here because it is half of what makes a picture reproducible
/// even though the page does not show the number any more (card 151), and
/// `speed` because the owner asked for it. The pipeline `output` is **not**:
/// the panel model, the dither and the limiter are about the panel, not about
/// the patch.
///
/// `params` holds only what differs from the patch's defaults, exactly as
/// [`PatchMemory`] does - which is also what makes "a parameter the patch has
/// gained since this was saved takes its default" true by construction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Setting {
    pub seed: u32,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, f32>,
    pub speed: f64,
}

impl Default for Setting {
    fn default() -> Self {
        Setting { seed: DEFAULT_SEED, params: BTreeMap::new(), speed: 1.0 }
    }
}

/// What one patch is set to right now: the **working copy**.
///
/// The same three things a [`Setting`] holds, which is what lets "modified" be
/// a comparison rather than a flag somebody has to remember to clear.
#[derive(Clone, Debug, PartialEq)]
pub struct Working {
    /// Only what differs from the patch's defaults; see [`sparse`].
    pub params: BTreeMap<String, f32>,
    pub seed: u32,
    pub speed: f64,
}

impl Working {
    /// This working copy as a setting to store.
    #[must_use]
    pub fn to_setting(&self) -> Setting {
        Setting { seed: self.seed, params: self.params.clone(), speed: self.speed }
    }
}

impl From<Setting> for Working {
    fn from(s: Setting) -> Working {
        Working { params: s.params, seed: s.seed, speed: s.speed }
    }
}

/// Is this name the read-only one?
#[must_use]
pub fn is_default_name(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case(DEFAULT_SETTING)
}

/// A name a person typed, as it will be stored - or the sentence to show them.
///
/// Trimmed, 1..=[`MAX_NAME_CHARS`] characters, not [`DEFAULT_SETTING`] in any
/// case, and nothing that is not printable: a name with a newline in it would
/// be a name nobody could see the whole of.
///
/// # Errors
///
/// If it is empty, too long, reserved, or has a control character in it.
pub fn check_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("A setting needs a name.".into());
    }
    let chars = name.chars().count();
    if chars > MAX_NAME_CHARS {
        return Err(format!("A setting's name can be at most {MAX_NAME_CHARS} characters; that one is {chars}."));
    }
    if name.chars().any(char::is_control) {
        return Err("A setting's name cannot have line breaks or control characters in it.".into());
    }
    if is_default_name(name) {
        return Err(format!("`{DEFAULT_SETTING}` is the patch's own setting, so it cannot be the name of one of yours."));
    }
    Ok(name.to_string())
}

/// Everything that survives a restart.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Persisted {
    /// Zero when the file did not say, which is a file older than this scheme
    /// and is migrated rather than thrown away.
    #[serde(default = "no_version")]
    pub version: u32,
    /// Devices, keyed by their stable id. A collection from day one even
    /// though one panel is the expected case (`studio-vision.md`, decision 3).
    pub devices: Vec<StoredDevice>,
    /// One per device. A player may exist for a device that is not currently
    /// reachable - that is the normal case after a power cut - and exactly one
    /// may be [`UNBOUND`], which is the studio that has not met a panel yet.
    pub players: Vec<StoredPlayer>,
    /// Which player the page is a window onto. [`UNBOUND`] (the empty string)
    /// while no panel is attached. With one panel - the expected case - this
    /// is that panel's id and nobody ever has to think about it.
    #[serde(default)]
    pub focus: String,
    /// What every patch was last left set to, anywhere in the studio
    /// (card 165). One map for the whole studio, keyed by patch id.
    ///
    /// Called `pieces` up to v3; [`load`] lifts it out of the raw JSON under
    /// either name before serde sees the file, because one bad value in it
    /// must cost that value and not the whole file.
    #[serde(alias = "pieces", skip_serializing_if = "BTreeMap::is_empty")]
    pub patches: Memory,
}

impl Default for Persisted {
    fn default() -> Self {
        Persisted {
            version: SCHEMA_VERSION,
            devices: Vec::new(),
            players: Vec::new(),
            focus: UNBOUND.to_string(),
            patches: Memory::new(),
        }
    }
}

/// The device id of a player that has no panel yet.
///
/// A studio always has at least one player, because the page always has a
/// picture to show. Before a panel is found that player is *unbound*: it
/// renders, it fills the page, and it has no link. When a panel turns up the
/// same player is renamed onto it, so the picture does not restart.
pub const UNBOUND: &str = "";

/// A device as the store remembers it.
///
/// **Keyed by `id`**, which is the device's own stable identifier (the `id=`
/// TXT key, or what `GET_INFO` answers), never by IP: a panel that takes a new
/// DHCP lease is the same panel. Until the studio has spoken to a manually
/// added address it does not know that id, so it uses a provisional one -
/// `addr:...` or `name:...` - and adopts the real one when it learns it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredDevice {
    pub id: String,
    /// What to call it here. Empty means "use the device's own name".
    pub name: String,
    /// DNS-SD instance name, if it has ever been seen on mDNS. This is what a
    /// link is pointed at when a human typed a name, because re-resolving it
    /// follows the device across a DHCP lease.
    pub instance: String,
    /// What a human typed, if they typed anything: a name or an `IP[:PORT]`.
    pub address: String,
    /// True when a human added it rather than discovery finding it. A manual
    /// device is never forgotten by a browse that does not see it.
    pub manual: bool,
}

/// What one panel plays - and, since card 170, what the page shows, because
/// they are the same thing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredPlayer {
    /// The device id this plays to, or [`UNBOUND`] for the player that has no
    /// panel yet.
    pub device: String,
    /// **Panel output.** False releases the link - the panel goes back to its
    /// own idle screen - and the player keeps rendering, because the page is
    /// still showing the patch.
    pub on: bool,
    /// `piece` up to v3 (card 150).
    #[serde(alias = "piece")]
    pub patch: String,
    pub seed: u32,
    /// Only the values that differ from the patch's defaults; the rest come
    /// from the patch's own spec every time it is built.
    pub params: BTreeMap<String, f32>,
    /// How a frame is finished for the panel. `settings` up to v3 (card 150).
    #[serde(alias = "settings")]
    pub output: Output,
    /// Brightness policy: a fixed level to apply whenever the link comes up,
    /// or `None` to leave whatever the device has. Never raised above the cap
    /// the device reports back.
    pub brightness: Option<u8>,
    /// Card 170: what used to be the design view's playback state. A paused
    /// patch is paused on the panel too - one picture, one answer.
    pub paused: bool,
    pub speed: f64,
}

impl Default for StoredPlayer {
    fn default() -> Self {
        StoredPlayer {
            device: String::new(),
            on: true,
            patch: default_patch().to_string(),
            seed: 1,
            params: BTreeMap::new(),
            output: Output::default(),
            brightness: None,
            paused: false,
            speed: 1.0,
        }
    }
}

/// The `preview` block of a v1 or v2 file: the design view's own patch, before
/// card 170 made the page a window onto the panel instead.
///
/// **Read, never written.** It exists so the migration can carry what a
/// deployed service was showing into the new shape rather than dropping it.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
struct LegacyPreview {
    /// A v1/v2 file spells these `piece` and `settings`; nothing else ever
    /// wrote this block, so the aliases are all there is to read.
    #[serde(alias = "piece")]
    patch: String,
    seed: u32,
    params: BTreeMap<String, f32>,
    #[serde(alias = "settings")]
    output: Output,
    paused: bool,
    speed: f64,
    /// "Send to panel", v1/v2's answer to the question card 170 replaces with
    /// the player's own `on`.
    panel_on: bool,
    /// A device id, a name or an address.
    panel_to: String,
}

impl Default for LegacyPreview {
    fn default() -> Self {
        LegacyPreview {
            patch: default_patch().to_string(),
            seed: 0,
            params: BTreeMap::new(),
            output: Output::default(),
            paused: false,
            speed: 1.0,
            panel_on: false,
            panel_to: String::new(),
        }
    }
}

// ----------------------------------------------- the per-patch memory (165) ---

/// What the studio remembers about one patch: its **working copy**, the name
/// that came from, and its **named settings** (card 151).
///
/// Only what *differs* from the patch's defaults is kept, so a patch whose
/// defaults improve in a later release improves for everybody who never
/// touched that parameter - and the file stays small.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PatchMemory {
    /// The seed this patch was last left on. `None` means "never chosen", and
    /// switching to it then puts it on [`DEFAULT_SEED`] - which is to say, on
    /// Default, where a patch nobody has touched belongs (card 151; before it,
    /// the patch kept whatever seed the previous one happened to be on).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u32>,
    /// Parameter values that differ from the defaults, by param id.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, f32>,
    /// How fast this patch was last played (card 151, v5). `None` means "never
    /// said", and switching to it leaves the player's speed alone.
    ///
    /// Speed is here rather than only on the player because a **setting**
    /// carries it: without this, loading a setting that plays at 0.4x and
    /// switching patch and back would quietly lose the 0.4x, and the setting
    /// would then read as modified for ever.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
    /// The named setting the working copy was loaded from. Empty means
    /// [`DEFAULT_SETTING`], which is where everything starts.
    ///
    /// **Whether it has been changed since is not stored**: that is the working
    /// copy above compared with this setting ([`modified`]), so it cannot be
    /// left behind by a change that forgot to clear a flag.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub setting: String,
    /// This patch's named settings, by name as it was typed.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, Setting>,
}

impl PatchMemory {
    /// Nothing worth writing down.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seed.is_none() && self.params.is_empty() && self.speed.is_none() && self.setting.is_empty() && self.settings.is_empty()
    }
}

/// The patch memory: patch id -> what that patch was left set to.
///
/// **One of these for the whole studio**, shared by the design view and by
/// every panel's player. Tuning a patch anywhere updates it; switching to a
/// patch anywhere restores from it. (The card asked for one per context; the
/// orchestrator reversed that on 2026-09-19 after the owner explained that the
/// browser is meant to be a window onto what the panel is doing, and that the
/// preview/player split is being unified in card 170. A per-context memory
/// would have been built for a distinction that is about to go away.)
pub type Memory = BTreeMap<String, PatchMemory>;

/// The one memory, as the engine and every player hold it.
///
/// A handle rather than a field: there is exactly one map, and everything that
/// tunes a patch writes to it. The lock is only ever taken *inside* one of
/// these methods, and never while another of the studio's locks is being
/// taken, so it cannot be half of a deadlock.
#[derive(Clone, Default)]
pub struct SharedMemory(Arc<Mutex<Memory>>);

impl SharedMemory {
    #[must_use]
    pub fn new(memory: Memory) -> SharedMemory {
        SharedMemory(Arc::new(Mutex::new(memory)))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Memory> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// What to write to the state file.
    #[must_use]
    pub fn snapshot(&self) -> Memory {
        self.lock().clone()
    }

    /// See [`remember`].
    pub fn remember(&self, def: &PatchDef, params: &BTreeMap<String, f32>, seed: u32, speed: f64) {
        remember(&mut self.lock(), def, params, seed, speed);
    }

    /// See [`recall`].
    #[must_use]
    pub fn recall(&self, def: &PatchDef, who: &str) -> Recalled {
        recall(&mut self.lock(), def, who)
    }

    /// See [`forget_params`].
    pub fn forget_params(&self, patch: &str) {
        forget_params(&mut self.lock(), patch);
    }

    /// Whether anything is remembered for this patch at all.
    #[must_use]
    pub fn knows(&self, patch: &str) -> bool {
        self.lock().contains_key(patch)
    }

    // ---- named settings (card 151) ----

    /// The names of this patch's settings, alphabetically. Never includes
    /// [`DEFAULT_SETTING`], which is not stored.
    #[must_use]
    pub fn setting_names(&self, patch: &str) -> Vec<String> {
        self.lock().get(patch).map(|e| e.settings.keys().cloned().collect()).unwrap_or_default()
    }

    /// The name the working copy was loaded from; [`DEFAULT_SETTING`] when it
    /// has not been loaded from anything.
    #[must_use]
    pub fn current_setting(&self, patch: &str) -> String {
        current_setting(&self.lock(), patch)
    }

    /// See [`usable_setting`].
    ///
    /// # Errors
    ///
    /// If this patch has no setting by that name.
    pub fn usable_setting(&self, def: &PatchDef, name: &str) -> Result<(Working, Vec<String>), String> {
        usable_setting(&self.lock(), def, name)
    }

    /// Put the working copy on a setting: what to play, and whatever had to be
    /// repaired on the way (said once by the caller, in `repaired` style).
    ///
    /// # Errors
    ///
    /// If this patch has no setting by that name.
    pub fn load_setting(&self, def: &PatchDef, name: &str) -> Result<(Working, Vec<String>), String> {
        let mut memory = self.lock();
        let (work, repaired) = usable_setting(&memory, def, name)?;
        remember(&mut memory, def, &work.params, work.seed, work.speed);
        let entry = memory.entry(def.id.to_string()).or_default();
        entry.setting = if is_default_name(name) { String::new() } else { name.to_string() };
        Ok((work, repaired))
    }

    /// See [`save_setting`].
    ///
    /// # Errors
    ///
    /// If the name is not one a setting may have, is already taken by another
    /// setting, or there are already [`MAX_SETTINGS`] of them.
    pub fn save_setting(&self, def: &PatchDef, name: Option<&str>, work: &Working) -> Result<String, String> {
        save_setting(&mut self.lock(), def, name, work)
    }

    /// See [`rename_setting`].
    ///
    /// # Errors
    ///
    /// If there is no such setting, it is Default, or the new name is not one
    /// a setting may have.
    pub fn rename_setting(&self, def: &PatchDef, from: Option<&str>, to: &str) -> Result<String, String> {
        rename_setting(&mut self.lock(), def, from, to)
    }

    /// See [`delete_setting`].
    ///
    /// # Errors
    ///
    /// If there is no such setting, or it is Default.
    pub fn delete_setting(&self, def: &PatchDef, name: Option<&str>) -> Result<String, String> {
        delete_setting(&mut self.lock(), def, name)
    }

    /// See [`modified`].
    #[must_use]
    pub fn modified(&self, def: &PatchDef, work: &Working) -> bool {
        modified(&self.lock(), def, work)
    }
}

impl std::fmt::Debug for SharedMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SharedMemory({} patches)", self.lock().len())
    }
}

/// The values worth writing down: what differs from the patch's defaults.
///
/// A value equal to the patch's default is *dropped* rather than stored: that
/// is the whole reason a later release's better default still reaches the
/// people who never touched that slider. It is also what makes comparing a
/// working copy with a setting an honest comparison - both sides have been
/// through here - and what makes "a parameter the patch has gained since this
/// was saved takes its default" true without any code for it.
#[must_use]
pub fn sparse(def: &PatchDef, params: &BTreeMap<String, f32>) -> BTreeMap<String, f32> {
    let mut out = BTreeMap::new();
    for spec in def.params {
        if let Some(v) = params.get(spec.id) {
            let v = spec.sanitise(*v);
            // `!=` on f32 is exactly right here: the question is whether this
            // is still literally the default, not whether it is close to it.
            if v != spec.default {
                out.insert(spec.id.to_string(), v);
            }
        }
    }
    out
}

/// Write down what `def` is set to now: the working copy.
pub fn remember(memory: &mut Memory, def: &PatchDef, params: &BTreeMap<String, f32>, seed: u32, speed: f64) {
    let params = sparse(def, params);
    let entry = memory.entry(def.id.to_string()).or_default();
    entry.seed = Some(seed);
    entry.params = params;
    if speed.is_finite() {
        entry.speed = Some(speed);
    }
}

/// Forget the parameters remembered for `patch`, keeping its seed.
///
/// This is what "Reset" means: back to the defaults, and *stay* there, rather
/// than being handed the old values again on the next switch back.
pub fn forget_params(memory: &mut Memory, patch: &str) {
    if let Some(entry) = memory.get_mut(patch) {
        entry.params.clear();
        if entry.is_empty() {
            memory.remove(patch);
        }
    }
}

/// What `def` should be set to, according to this memory.
///
/// Everything the card asks for about a value this build cannot use happens
/// here, per value, and never costs the rest of the entry:
///
/// | in the file | what happens |
/// |---|---|
/// | a parameter this build's patch does not have | ignored |
/// | a value outside the spec's range | clamped to the range |
/// | a value that is not a finite number | the patch's default |
///
/// Corrections are **written back** into the memory, so a file that needed
/// fixing is fixed once rather than complained about on every switch - which
/// is also what makes "logged once" true without a set of things already said.
/// `who` names the context for that one log line.
#[must_use]
pub fn recall(memory: &mut Memory, def: &PatchDef, who: &str) -> Recalled {
    let Some(entry) = memory.get_mut(def.id) else {
        return Recalled::default();
    };
    let (usable, repaired) = usable_params(&entry.params, def.params);
    if !repaired.is_empty() {
        eprintln!("studio: {who}: what was remembered for `{}`: {}", def.id, repaired.join("; "));
        entry.params = usable.clone();
        if entry.is_empty() {
            memory.remove(def.id);
            return Recalled { params: usable, seed: None, speed: None };
        }
    }
    let entry = memory.get(def.id);
    Recalled { params: usable, seed: entry.and_then(|e| e.seed), speed: entry.and_then(|e| e.speed) }
}

/// What the memory had to say about a patch. `None` is "never said", and the
/// caller then leaves that of its own alone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recalled {
    pub params: BTreeMap<String, f32>,
    pub seed: Option<u32>,
    pub speed: Option<f64>,
}

/// The remembered values this build can actually use, and one sentence per
/// value it had to correct.
#[must_use]
pub fn usable_params(remembered: &BTreeMap<String, f32>, specs: &[ParamSpec]) -> (BTreeMap<String, f32>, Vec<String>) {
    let mut usable = BTreeMap::new();
    let mut repaired = Vec::new();
    for (id, value) in remembered {
        let Some(spec) = specs.iter().find(|s| s.id == id) else {
            repaired.push(format!("`{id}` is not one of its parameters any more"));
            continue;
        };
        if !value.is_finite() {
            repaired.push(format!("`{id}` was not a number; back to {}", spec.default));
            continue;
        }
        let fixed = spec.sanitise(*value);
        if fixed != *value {
            repaired.push(format!("`{id}` was {value}, outside {}..{}; held at {fixed}", spec.min, spec.max));
        }
        // A value that is now exactly the default is not worth remembering.
        if fixed != spec.default {
            usable.insert(spec.id.to_string(), fixed);
        }
    }
    (usable, repaired)
}

// ------------------------------------ named settings (card 151) ------------

/// The name the working copy of `patch` was loaded from, or
/// [`DEFAULT_SETTING`].
#[must_use]
pub fn current_setting(memory: &Memory, patch: &str) -> String {
    match memory.get(patch).map(|e| e.setting.as_str()) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => DEFAULT_SETTING.to_string(),
    }
}

/// The setting `name` **as this build can use it**, and one sentence per value
/// it could not use as written.
///
/// This is where a patch that has changed since a setting was saved is dealt
/// with, and it is deliberately the *only* place: a parameter the patch has
/// lost is dropped, one outside its range is clamped, one that is not a number
/// goes back to the default, and one the patch has gained is simply absent and
/// therefore already its default.
///
/// It is also what `modified` compares against, rather than the raw stored
/// setting - otherwise a setting that needed repairing would read as modified
/// from the moment it was loaded, for ever.
///
/// # Errors
///
/// If this patch has no setting by that name.
pub fn usable_setting(memory: &Memory, def: &PatchDef, name: &str) -> Result<(Working, Vec<String>), String> {
    if is_default_name(name) {
        return Ok((Working { params: BTreeMap::new(), seed: DEFAULT_SEED, speed: 1.0 }, Vec::new()));
    }
    let stored = memory
        .get(def.id)
        .and_then(|e| e.settings.get(name.trim()))
        .ok_or_else(|| format!("`{}` has no setting called `{}`.", def.name, name.trim()))?;
    let (params, mut repaired) = usable_params(&stored.params, def.params);
    let speed = if stored.speed.is_finite() && stored.speed > 0.0 {
        stored.speed
    } else {
        repaired.push(format!("the speed saved in `{}` was not a speed; back to 1.00x", name.trim()));
        1.0
    };
    // Say which setting each sentence is about: `repaired` is read on a
    // dashboard beside sentences about the working copy.
    let what = name.trim();
    for line in &mut repaired {
        if !line.contains(what) {
            *line = format!("in `{what}`: {line}");
        }
    }
    Ok((Working { params, seed: stored.seed, speed }, repaired))
}

/// **Is the working copy still the setting it says it is?**
///
/// Computed, never stored (the card's word). Both sides go through the same
/// two funnels - [`sparse`] for the values and [`usable_setting`] for the
/// stored one - so this is an honest comparison and not a flag anybody has to
/// remember to clear. A setting that has gone missing under the working copy's
/// feet counts as modified: there is nothing left to be equal to.
#[must_use]
pub fn modified(memory: &Memory, def: &PatchDef, work: &Working) -> bool {
    let name = current_setting(memory, def.id);
    match usable_setting(memory, def, &name) {
        Ok((was, _)) => was != *work,
        Err(_) => true,
    }
}

/// Save the working copy as a setting.
///
/// `name` of `None` is **Save**: overwrite the one the working copy was loaded
/// from. A name is **Save as...**, and a name that is already a setting's,
/// exactly, overwrites it. Either way the working copy ends up loaded from the
/// setting just written, so it is not modified the moment it is saved.
///
/// Returns the name it was saved under.
///
/// # Errors
///
/// If the name is not one a setting may have ([`check_name`]), if another
/// setting has it but spelled differently, if there is nothing to overwrite
/// (Default, which is read-only), or if there are already [`MAX_SETTINGS`].
pub fn save_setting(memory: &mut Memory, def: &PatchDef, name: Option<&str>, work: &Working) -> Result<String, String> {
    let name = match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(asked) => check_name(asked)?,
        // Save with no name: the one it is on.
        None => {
            let current = current_setting(memory, def.id);
            if is_default_name(&current) {
                return Err(format!(
                    "`{DEFAULT_SETTING}` is the patch's own setting and cannot be written over; save this under a name of your own."
                ));
            }
            current
        }
    };
    let entry = memory.entry(def.id.to_string()).or_default();
    // A name that differs only in case from one already here is a second
    // setting nobody could tell from the first.
    if let Some(clash) = entry.settings.keys().find(|k| k.eq_ignore_ascii_case(&name) && **k != name) {
        return Err(format!("There is already a setting called `{clash}`."));
    }
    if !entry.settings.contains_key(&name) && entry.settings.len() >= MAX_SETTINGS {
        return Err(format!("`{}` already has {MAX_SETTINGS} settings, which is as many as it may have.", def.name));
    }
    entry.settings.insert(name.clone(), work.to_setting());
    entry.setting.clone_from(&name);
    Ok(name)
}

/// Rename a setting. `from` of `None` is the one the working copy is on.
///
/// Returns the new name.
///
/// # Errors
///
/// If there is no such setting, if it is Default (which is not yours to
/// rename), or if the new name is not one a setting may have.
pub fn rename_setting(memory: &mut Memory, def: &PatchDef, from: Option<&str>, to: &str) -> Result<String, String> {
    let from = named_setting(memory, def, from)?;
    let to = check_name(to)?;
    let entry = memory.entry(def.id.to_string()).or_default();
    if to != from {
        if let Some(clash) = entry.settings.keys().find(|k| k.eq_ignore_ascii_case(&to) && **k != from) {
            return Err(format!("There is already a setting called `{clash}`."));
        }
    }
    let Some(setting) = entry.settings.remove(&from) else {
        return Err(format!("`{}` has no setting called `{from}`.", def.name));
    };
    entry.settings.insert(to.clone(), setting);
    if entry.setting == from {
        entry.setting.clone_from(&to);
    }
    Ok(to)
}

/// Delete a setting. `name` of `None` is the one the working copy is on.
///
/// **What is playing does not change.** The values stay exactly where they
/// are; what goes is the name they came from, so the working copy is then on
/// Default and reads as modified - which is the truth.
///
/// Returns the name deleted.
///
/// # Errors
///
/// If there is no such setting, or it is Default.
pub fn delete_setting(memory: &mut Memory, def: &PatchDef, name: Option<&str>) -> Result<String, String> {
    let name = named_setting(memory, def, name)?;
    let entry = memory.entry(def.id.to_string()).or_default();
    if entry.settings.remove(&name).is_none() {
        return Err(format!("`{}` has no setting called `{name}`.", def.name));
    }
    if entry.setting == name {
        entry.setting.clear();
    }
    if entry.is_empty() {
        memory.remove(def.id);
    }
    Ok(name)
}

/// Which setting a rename or a delete is about: the one named, or the one the
/// working copy is on - and never Default, which is nobody's to change.
fn named_setting(memory: &Memory, def: &PatchDef, name: Option<&str>) -> Result<String, String> {
    let name = match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => n.to_string(),
        None => current_setting(memory, def.id),
    };
    if is_default_name(&name) {
        return Err(format!("`{DEFAULT_SETTING}` is the patch's own setting: it cannot be renamed or deleted."));
    }
    Ok(name)
}

/// Read the `patches` object out of a file as forgivingly as the card asks.
///
/// This runs on `serde_json::Value` rather than through serde's `f32`
/// deliberately: `BTreeMap<String, f32>` refuses a `null`, a string or an
/// object, and a refusal here would condemn the **whole file** to
/// `state.bad.json` under card 106's rules. One bad value must cost exactly
/// that one value.
fn clean_memory(raw: Option<&serde_json::Value>, repaired: &mut Vec<String>) -> Memory {
    let Some(serde_json::Value::Object(entries)) = raw else {
        if raw.is_some_and(|v| !v.is_null()) {
            repaired.push("what was remembered was not an object; forgotten".into());
        }
        return Memory::new();
    };
    let mut memory = Memory::new();
    let mut unknown = 0usize;
    for (patch, value) in entries {
        // An unknown patch id keeps its entry - a patch that comes back in a
        // later release gets its memory back - but only so many of them.
        if screeny_art::patch::find(patch).is_none() {
            unknown += 1;
            if unknown > MAX_UNKNOWN_PATCHES {
                repaired.push(format!("`{patch}` is not a patch here and there were already {MAX_UNKNOWN_PATCHES} such entries; dropped"));
                continue;
            }
        }
        let serde_json::Value::Object(entry) = value else {
            repaired.push(format!("what was remembered for `{patch}` was not an object; forgotten"));
            continue;
        };
        let seed = match entry.get("seed") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => match v.as_u64().and_then(|n| u32::try_from(n).ok()) {
                Some(n) => Some(n),
                None => {
                    repaired.push(format!("the seed remembered for `{patch}` was not a seed; forgotten"));
                    None
                }
            },
        };
        let params = clean_params(entry.get("params"), &format!("`{patch}`'s remembered"), repaired);
        let speed = clean_speed(entry.get("speed"), &format!("the speed remembered for `{patch}`"), repaired);
        // Card 151. The name the working copy was loaded from, and the named
        // settings themselves - read one at a time for the same reason as
        // everything above: one unreadable setting costs that setting.
        let setting = match entry.get("setting") {
            None | Some(serde_json::Value::Null) => String::new(),
            Some(serde_json::Value::String(s)) => s.trim().to_string(),
            Some(_) => {
                repaired.push(format!("the setting `{patch}` was on was not a name; it is on {DEFAULT_SETTING}"));
                String::new()
            }
        };
        let settings = clean_settings(patch, entry.get("settings"), repaired);
        // A name that is not there any more is not a name it is on.
        let setting = if setting.is_empty() || settings.contains_key(&setting) {
            setting
        } else {
            repaired.push(format!("`{patch}` was on a setting called `{setting}`, which is not in the file; it is on {DEFAULT_SETTING}"));
            String::new()
        };
        let m = PatchMemory { seed, params, speed, setting, settings };
        if !m.is_empty() {
            memory.insert(patch.clone(), m);
        }
    }
    memory
}

/// A `params` object out of the file, one value at a time.
fn clean_params(raw: Option<&serde_json::Value>, whose: &str, repaired: &mut Vec<String>) -> BTreeMap<String, f32> {
    let mut params = BTreeMap::new();
    if let Some(serde_json::Value::Object(ps)) = raw {
        for (id, v) in ps {
            match v.as_f64().map(|f| f as f32).filter(|f| f.is_finite()) {
                Some(f) => {
                    params.insert(id.clone(), f);
                }
                None => repaired.push(format!("{whose} `{id}` was not a number; back to its default")),
            }
        }
    } else if raw.is_some_and(|v| !v.is_null()) {
        repaired.push(format!("{whose} parameters were not an object; forgotten"));
    }
    params
}

/// A `speed` out of the file. Absent is "never said"; anything that is not a
/// speed is said once and forgotten.
fn clean_speed(raw: Option<&serde_json::Value>, whose: &str, repaired: &mut Vec<String>) -> Option<f64> {
    match raw {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => match v.as_f64().filter(|f| f.is_finite() && *f > 0.0) {
            Some(f) => Some(f),
            None => {
                repaired.push(format!("{whose} was not a speed; forgotten"));
                None
            }
        },
    }
}

/// The `settings` object of one patch (card 151).
///
/// Bounded ([`MAX_SETTINGS`]) and checked by the same name rules a person's
/// typing goes through, because this file can be hand-edited and a name nobody
/// can see the whole of is no use to anybody.
fn clean_settings(patch: &str, raw: Option<&serde_json::Value>, repaired: &mut Vec<String>) -> BTreeMap<String, Setting> {
    let mut out = BTreeMap::new();
    let Some(serde_json::Value::Object(entries)) = raw else {
        if raw.is_some_and(|v| !v.is_null()) {
            repaired.push(format!("the settings saved for `{patch}` were not an object; forgotten"));
        }
        return out;
    };
    for (name, value) in entries {
        let name = match check_name(name) {
            Ok(n) => n,
            Err(why) => {
                repaired.push(format!("a setting of `{patch}` was dropped: {why}"));
                continue;
            }
        };
        if out.len() >= MAX_SETTINGS {
            repaired.push(format!("`{patch}` had more than {MAX_SETTINGS} settings; `{name}` and any after it were dropped"));
            break;
        }
        // A second name that differs only in case is one nobody could tell
        // from the first.
        if let Some(first) = out.keys().find(|k: &&String| k.eq_ignore_ascii_case(&name)) {
            repaired.push(format!("`{patch}` had two settings called `{first}`; the second was dropped"));
            continue;
        }
        let serde_json::Value::Object(setting) = value else {
            repaired.push(format!("the setting `{name}` of `{patch}` was not an object; dropped"));
            continue;
        };
        let seed = match setting.get("seed") {
            None | Some(serde_json::Value::Null) => DEFAULT_SEED,
            Some(v) => match v.as_u64().and_then(|n| u32::try_from(n).ok()) {
                Some(n) => n,
                None => {
                    repaired.push(format!("the seed saved in `{name}` (`{patch}`) was not a seed; {DEFAULT_SEED} instead"));
                    DEFAULT_SEED
                }
            },
        };
        let params = clean_params(setting.get("params"), &format!("`{name}`'s saved"), repaired);
        let speed = clean_speed(setting.get("speed"), &format!("the speed saved in `{name}` (`{patch}`)"), repaired).unwrap_or(1.0);
        out.insert(name, Setting { seed, params, speed });
    }
    out
}

/// Card 102: a file's `settings.levels` is not a setting any more.
///
/// It named a number of levels per channel, with 64, 32 and 16 to choose from.
/// 32 and 16 were "the panel when it is dimmed", which this device has not
/// done since card 020 - it dims the output-enable window and keeps every duty
/// step - and 64 was the panel before its temporal dither. The setting is now
/// `output.panel`, one of `dithered`, `bit_planes` or `aligned_dark`.
///
/// A file that still names `levels` **loads**: serde ignores the key and the
/// panel comes up at its default, which is the device. This is only how the
/// studio *says so*, once, in the same breath as everything else it had to
/// correct - the alternative is a setting that silently stops meaning what the
/// person chose. No schema bump: nothing about the file's shape changed.
fn note_retired_levels(raw: &serde_json::Value, repaired: &mut Vec<String>) {
    let mut found: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // The block was `settings` up to v3 and is `output` from v4 (card 150);
    // a file of either shape may still name the retired key.
    let mut look = |block: Option<&serde_json::Value>| {
        let block = block.and_then(|b| b.get("output").or_else(|| b.get("settings")));
        if let Some(v) = block.and_then(|s| s.get("levels")) {
            found.insert(v.to_string());
        }
    };
    look(raw.get("preview"));
    if let Some(serde_json::Value::Array(players)) = raw.get("players") {
        for p in players {
            look(Some(p));
        }
    }
    if found.is_empty() {
        return;
    }
    let values: Vec<&str> = found.iter().map(String::as_str).collect();
    repaired.push(format!(
        "`settings.levels` ({}) is not a setting any more: 32 and 16 modelled a dimming this device has never done \
         and 64 was the panel before its temporal dither (card 102); the panel model is now `output.panel`, \
         which starts at `dithered` - what the device really shows",
        values.join(", ")
    ));
}

/// Card 161: a file's `fps` is not a setting any more.
///
/// A player used to carry its own frame rate, 1 to 60, and a fresh one started
/// at 60. There is one rate now - [`screeny_art::FPS`], the panel's own - and
/// nothing offers a choice, because the link folded half of a 60 fps player's
/// frames away and the picture was being sampled at 60 and shown at 30.
///
/// A file that still names `fps` **loads**: serde ignores the key and the
/// player renders at 30. This is only how the studio *says so*, once, beside
/// everything else it had to correct - exactly what card 102 did for
/// `settings.levels`. **No schema bump**, for card 102's reason: nothing about
/// the file's shape changed, a v5 file with an `fps` key is a perfectly good v5
/// file, and bumping would send every deployed studio through a migration and a
/// backup copy to delete one number. (It is dropped for good on the next save,
/// which is what writing the file has always done with keys this build has no
/// field for.)
fn note_retired_fps(raw: &serde_json::Value, repaired: &mut Vec<String>) {
    let mut found: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut look = |block: Option<&serde_json::Value>| {
        if let Some(v) = block.and_then(|b| b.get("fps")) {
            found.insert(v.to_string());
        }
    };
    // The design view's own block, up to v2, and every player.
    look(raw.get("preview"));
    if let Some(serde_json::Value::Array(players)) = raw.get("players") {
        for p in players {
            look(Some(p));
        }
    }
    if found.is_empty() {
        return;
    }
    let values: Vec<&str> = found.iter().map(String::as_str).collect();
    repaired.push(format!(
        "`fps` ({}) is not a setting any more: there is one rate, {} fps, which is the panel's own \
         (card 161) - rendering above it only fed the link frames it folded away",
        values.join(", "),
        screeny_art::FPS
    ));
}

/// A file that did not name a schema version at all.
fn no_version() -> u32 {
    0
}

/// The patch a fresh studio starts on.
#[must_use]
pub fn default_patch() -> &'static str {
    screeny_art::patches::ALL[0].id
}

/// What the store is doing, for `/api/v1/status` and for `/healthz`.
///
/// `last_error` is the one field that makes the server *unhealthy*: a studio
/// that cannot write its state will not come back as itself, which is the
/// whole promise. A panel being unplugged is not in here on purpose.
#[derive(Clone, Debug, Default, Serialize)]
pub struct StoreHealth {
    /// The file, or "(memory)" when nothing is persisted.
    pub path: String,
    pub persisting: bool,
    pub writes: u64,
    pub last_write_unix: Option<u64>,
    /// The last write that failed, and has not since succeeded.
    pub last_error: Option<String>,
    /// Why the studio started from a default rather than from the file, if it
    /// did. `None` covers both "the file was read" and "there was no file",
    /// which are both normal; [`StoreHealth::recovered`] tells them apart.
    pub recovered: Option<String>,
    /// Remembered values this build could not use as written and silently
    /// corrected (card 165). Not a fault, never a 503: a value being out of
    /// range after a patch was re-ranged is exactly what the memory is meant
    /// to survive. Capped at [`MAX_REPAIRS`].
    pub repaired: Vec<String>,
}

// ---------------------------------------------------------------- loading ---

/// What reading the file produced.
struct Loaded {
    state: Persisted,
    /// Why the studio started from a default rather than from the file.
    recovered: Option<String>,
    /// Remembered values that had to be corrected. Not a recovery: the file
    /// was used, one value in it was not.
    repaired: Vec<String>,
}

impl Loaded {
    fn fresh(why: Option<String>) -> Loaded {
        Loaded { state: Persisted::default(), recovered: why, repaired: Vec::new() }
    }
}

/// Read the state file, whatever is in it.
///
/// Returns the state to start from and, when the file could not be used, one
/// sentence saying why - already logged, and worth putting on the dashboard.
/// This function has no failure mode: that is the point of it.
fn load(path: &Path) -> Loaded {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        // No file is the normal first run, not a problem to report.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded::fresh(None),
        Err(e) => {
            return Loaded::fresh(Some(format!("{} could not be read ({e}); starting from defaults", path.display())));
        }
    };

    let keep_the_file = |why: String| {
        let bad = path.with_file_name(BAD_FILE);
        let moved = std::fs::rename(path, &bad).is_ok();
        let where_ = if moved { format!("; kept as {}", bad.display()) } else { String::new() };
        Loaded::fresh(Some(format!("{why}{where_}; starting from defaults")))
    };

    // One parse into a `Value` first. The version has to be read before the
    // shape is trusted - a file from a newer build may have fields this one
    // would reject - and the per-patch memory has to be lifted out and cleaned
    // before serde sees it, because a single bad value in there must cost that
    // value and not the whole file.
    let mut raw: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return keep_the_file(format!("the state file could not be parsed ({e})")),
    };

    if let Some(v) = raw.get("version").and_then(serde_json::Value::as_u64) {
        if v > u64::from(SCHEMA_VERSION) {
            let keep = path.with_file_name(backup_name(v));
            let moved = std::fs::rename(path, &keep).is_ok();
            let where_ = if moved { format!("; kept as {}", keep.display()) } else { String::new() };
            return Loaded::fresh(Some(format!(
                "the state file is schema v{v} and this build understands v{SCHEMA_VERSION}{where_}"
            )));
        }
    }

    let mut repaired = Vec::new();
    // `patches` from v4, `pieces` before it (card 150).
    let patches = clean_memory(raw.get("patches").or_else(|| raw.get("pieces")), &mut repaired);
    note_retired_levels(&raw, &mut repaired);
    note_retired_fps(&raw, &mut repaired);
    // The `preview` block of a v1/v2 file, lifted out for the same reason as
    // `patches`: this build's `Persisted` has no field for it, and it is read
    // forgivingly (a missing or malformed one is the default, never a reason
    // to condemn the file).
    let legacy: LegacyPreview = raw
        .get("preview")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    if let Some(o) = raw.as_object_mut() {
        o.remove("patches");
        o.remove("pieces");
        o.remove("preview");
    }

    let mut state: Persisted = match serde_json::from_value(raw) {
        Ok(p) => p,
        Err(e) => return keep_the_file(format!("the state file could not be parsed ({e})")),
    };
    let was = state.version;
    state.version = SCHEMA_VERSION;
    state.patches = patches;
    let kept = if was < SCHEMA_VERSION {
        let kept = back_up(path, was);
        migrate(&mut state, &legacy, was);
        kept
    } else {
        None
    };
    // After the migrations, so a v1/v2 file's player - which `migrate_to_v3`
    // builds out of the old `preview` block - is looked at too.
    repair_unknown_players(&mut state, &mut repaired);
    repaired.truncate(MAX_REPAIRS);
    let recovered = (was != SCHEMA_VERSION).then(|| {
        let where_ = kept.map_or(String::new(), |k| format!("; the v{was} file is kept as {k}"));
        format!("the state file was schema v{was}; migrated to v{SCHEMA_VERSION}{where_}")
    });
    Loaded { state, recovered, repaired }
}

/// **A player on a patch this build has not got comes up on the default
/// patch**, and the file says so once, in `repaired`.
///
/// Card 178 removed two patches, so this is no longer hypothetical: a deployed
/// studio whose panel was on `plasma` is restarted onto a build that has never
/// heard of it. Without this the player kept the missing id, `make_core` fell
/// back to something else to have a picture at all, and the page went on
/// naming a patch that was not what was playing - true only on stderr, at
/// startup, once.
///
/// Three things this deliberately does **not** do:
///
/// - It does not touch [`Persisted::patches`]. How somebody had `plasma` set is
///   theirs, they may have spent an evening on it, and a patch can come back;
///   `clean_memory` keeps unknown entries on purpose and this keeps that
///   promise. Only the player moves.
/// - It does not pick [`crate::player::fallback_patch`]. That is for a patch
///   that *broke* while running, where "not the one that just failed" is the
///   point. This is a patch that was never here, and the honest answer is the
///   one a studio with no file at all comes up on.
/// - It does not carry the old patch's seed, speed or parameters across. They
///   described a different picture. The default patch arrives set the way the
///   studio last left it, which is what switching to it by hand would do.
fn repair_unknown_players(state: &mut Persisted, repaired: &mut Vec<String>) {
    let Some(def) = screeny_art::patch::find(default_patch()) else {
        // Unreachable: `default_patch` is `ALL[0]`. Not worth a panic in a
        // function whose whole job is that starting up cannot fail.
        return;
    };
    for player in &mut state.players {
        if screeny_art::patch::find(&player.patch).is_some() {
            continue;
        }
        let whose = if player.device == UNBOUND { "the page".to_string() } else { format!("panel {}", player.device) };
        let was = std::mem::replace(&mut player.patch, def.id.to_string());
        let recalled = recall(&mut state.patches, def, &whose);
        player.params = recalled.params;
        player.seed = recalled.seed.unwrap_or(DEFAULT_SEED);
        player.speed = recalled.speed.unwrap_or(1.0);
        repaired.push(format!(
            "`{was}` is not a patch this build has, so {whose} is playing `{}`; \
             what was remembered for `{was}` is kept, in case it comes back",
            def.id
        ));
    }
}

/// Copy a file that is about to be migrated aside, so the version it was
/// written as survives the first save this build makes.
///
/// Best effort on purpose: a state directory that cannot be written to is a
/// reason to say so on the dashboard, never a reason not to start (rule 3 at
/// the top of this file). Returns the name it was kept under, for the
/// `recovered` sentence.
fn back_up(path: &Path, was: u32) -> Option<String> {
    let name = backup_name(u64::from(was));
    let to = path.with_file_name(&name);
    match std::fs::copy(path, &to) {
        Ok(_) => Some(name),
        Err(e) => {
            eprintln!("studio: state: the v{was} file could not be copied to {}: {e}", to.display());
            None
        }
    }
}

/// Bring a file older than [`SCHEMA_VERSION`] up to it.
///
/// - **v1/v2 -> v3** is [`migrate_to_v3`], and is the only one that moves
///   anything: it is run only for a file that really is older than v3, because
///   a v3 file has no `preview` block and feeding the migration an absent one
///   would write a memory entry for the default patch that nobody asked for.
/// - **v3 -> v4** (card 150) is a renaming and needs no code: `pieces`,
///   `piece` and `settings` are read by the `#[serde(alias)]`s on [`Persisted`],
///   [`StoredPlayer`] and [`LegacyPreview`] and by [`load`]'s own lookup, and
///   the next save writes `patches`, `patch` and `output`. The file is copied
///   to `state.v3.json` first ([`back_up`]) so the older build could still be
///   put back.
/// - **v4 -> v5** is [`migrate_to_v5`]: the named settings are new and empty,
///   and the one thing that has to *move* is speed.
///
/// Each step is behind its own `was <` guard, so a file that is already past a
/// step never runs it again - which is the property card 150 wrote the guard
/// for, and the reason a v1 file can pass through every step in one start.
fn migrate(state: &mut Persisted, preview: &LegacyPreview, was: u32) {
    if was < 3 {
        migrate_to_v3(state, preview, was);
    }
    if was < 5 {
        migrate_to_v5(state);
    }
}

/// v4 -> v5 (card 151): a patch's **speed** becomes part of what is remembered
/// about the patch, because a setting carries it.
///
/// Before this card speed was only a player's. So the patch each player was on
/// takes that player's speed as its working copy's, and a studio that was
/// playing at 0.4x comes back at 0.4x rather than snapping to 1.00x the first
/// time somebody switches patch and back.
///
/// **Fill, never overwrite**: a v1/v2 file has already been through
/// [`migrate_to_v3`], whose `remember` calls set a speed from the same places,
/// and running this over the top would undo that. With one panel - the expected
/// case - there is nothing to choose anyway.
///
/// **And only a speed somebody actually set.** A player at 1.00x has nothing
/// to say - that is where a working copy starts - and writing it down would
/// invent a memory entry for a patch nobody has ever tuned, which is the thing
/// `migrating_a_v3_file_invents_no_memory` exists to stop.
///
/// Nothing else moves. `settings` starts empty and `setting` starts as Default,
/// which is what "this patch has never been saved under a name" is.
fn migrate_to_v5(state: &mut Persisted) {
    for player in &state.players {
        if screeny_art::patch::find(&player.patch).is_none() {
            continue;
        }
        if !player.speed.is_finite() || player.speed <= 0.0 || player.speed == 1.0 {
            continue;
        }
        let entry = state.patches.entry(player.patch.clone()).or_default();
        if entry.speed.is_none() {
            entry.speed = Some(player.speed);
        }
    }
}

/// v1/v2 -> v3: the attached panel's player is the truth, and the design
/// view's own block goes away without taking anything with it.
///
/// Three rules, in this order:
///
/// 1. **The memory.** A v1 file has no `patches` map at all, so what each
///    context was playing becomes it - the preview first, then each player, so
///    **a panel's values win** on a patch both were on (card 165). A v2 file
///    already has the map and it is already right: the preview's values are
///    merged in only for a patch the map knows *nothing* about, because
///    overwriting there would undo exactly the merge v1 -> v2 did.
/// 2. **`panel_on` / `panel_to` are dropped** in favour of the player's `on`.
///    The one case where that would lose something is a file with no players
///    at all: the design view was the only thing playing. That becomes a
///    player - on the device `panel_to` names when the file knows it, unbound
///    otherwise - carrying the preview's patch, seed, parameters, output and
///    rate, so nothing that was playing stops playing.
/// 3. **`focus`** becomes the first player's device: with one panel, which is
///    the expected case, there is nothing to choose.
///
/// Because [`remember`] drops anything equal to the patch's default, a v1
/// file's full parameter dump comes out of the migration as just the values
/// that were actually moved.
fn migrate_to_v3(state: &mut Persisted, preview: &LegacyPreview, was: u32) {
    if was < 2 {
        if let Some(def) = screeny_art::patch::find(&preview.patch) {
            remember(&mut state.patches, def, &preview.params, preview.seed, preview.speed);
        }
        // Second, so a panel overwrites the design view on a shared patch.
        for player in &state.players {
            if let Some(def) = screeny_art::patch::find(&player.patch) {
                remember(&mut state.patches, def, &player.params, player.seed, player.speed);
            }
        }
    } else if let Some(def) = screeny_art::patch::find(&preview.patch) {
        // v2: the map is the answer; fill a gap, never overwrite one.
        if !state.patches.contains_key(def.id) {
            remember(&mut state.patches, def, &preview.params, preview.seed, preview.speed);
        }
    }

    if state.players.is_empty() {
        // Nothing was configured to play, so the design view was all there
        // was. Carry it over whole; the page comes back showing it.
        let device = preview
            .panel_on
            .then(|| device_named(&state.devices, &preview.panel_to))
            .flatten()
            .unwrap_or_else(|| UNBOUND.to_string());
        state.players.push(StoredPlayer {
            device,
            on: preview.panel_on,
            patch: preview.patch.clone(),
            seed: preview.seed,
            params: preview.params.clone(),
            output: preview.output,
            brightness: None,
            paused: preview.paused,
            speed: preview.speed,
        });
    }

    if state.focus.is_empty() {
        if let Some(first) = state.players.first() {
            state.focus.clone_from(&first.device);
        }
    }
}

/// The id of the device a v1/v2 `panel_to` was naming, if this file knows it.
///
/// `panel_to` was whatever a human typed - an id, an mDNS instance name or an
/// address - so all three are tried. `None` means the file has no device by
/// that name, in which case the migrated player is left unbound rather than
/// pointed at a device that does not exist.
fn device_named(devices: &[StoredDevice], to: &str) -> Option<String> {
    let to = to.trim();
    if to.is_empty() {
        return None;
    }
    devices
        .iter()
        .find(|d| d.id == to || d.instance == to || d.address == to)
        .map(|d| d.id.clone())
}

// ---------------------------------------------------------------- writing ---

struct Slot {
    /// The newest state waiting to be written. One slot: a burst of changes
    /// costs one write, and a save never waits for the disk.
    pending: Option<Persisted>,
    stop: bool,
    /// Saves asked for, and saves dealt with. `flush` waits for the second to
    /// catch up with the first.
    ///
    /// A pair rather than a single counter because coalescing makes them jump:
    /// three saves and one write is the normal case, and a `flush` that only
    /// looked at "is anything pending" would return in the gap between the
    /// writer taking the work and finishing it.
    queued: u64,
    done: u64,
}

struct Inner {
    path: Option<PathBuf>,
    slot: Mutex<Slot>,
    wake: Condvar,
    done: Condvar,
    health: Mutex<StoreHealth>,
}

/// The state file, and the one thread that writes it.
pub struct Store {
    inner: Arc<Inner>,
    writer: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Store {
    /// Open the store in `dir`, creating the directory if it is not there, and
    /// read whatever state is in it.
    ///
    /// `dir` of `None` keeps everything in memory: nothing is written and
    /// nothing is read. That is what most tests want, and it is why a test can
    /// never leave a file behind.
    #[must_use]
    pub fn open(dir: Option<&Path>) -> (Arc<Store>, Persisted) {
        let mut health = StoreHealth { path: "(memory)".into(), ..StoreHealth::default() };
        let (path, loaded) = match dir {
            None => (None, Persisted::default()),
            Some(dir) => {
                let path = dir.join(FILE);
                health.path = path.display().to_string();
                health.persisting = true;
                if let Err(e) = std::fs::create_dir_all(dir) {
                    health.last_error = Some(format!("creating {}: {e}", dir.display()));
                    eprintln!("studio: state: creating {}: {e}", dir.display());
                    // Carry on in memory rather than refusing to start. The
                    // server is still useful; `/healthz` says it is not well.
                    (Some(path), Persisted::default())
                } else {
                    let loaded = load(&path);
                    if let Some(why) = &loaded.recovered {
                        eprintln!("studio: state: {why}");
                    }
                    // Said once, here, on the way in - not once per switch.
                    for what in &loaded.repaired {
                        eprintln!("studio: state: {what}");
                    }
                    health.recovered = loaded.recovered;
                    health.repaired = loaded.repaired;
                    (Some(path), loaded.state)
                }
            }
        };

        let inner = Arc::new(Inner {
            path,
            slot: Mutex::new(Slot { pending: None, stop: false, queued: 0, done: 0 }),
            wake: Condvar::new(),
            done: Condvar::new(),
            health: Mutex::new(health),
        });
        let writer = if dir.is_some() {
            let inner2 = Arc::clone(&inner);
            std::thread::Builder::new()
                .name("state".into())
                .spawn(move || write_loop(&inner2))
                .ok()
        } else {
            None
        };
        (Arc::new(Store { inner, writer: Mutex::new(writer) }), loaded)
    }

    /// Ask for this state to be on disk. Never blocks, never fails: the worst
    /// case is that the write fails and `/healthz` says so.
    pub fn save(&self, state: Persisted) {
        let mut slot = self.inner.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.stop {
            return;
        }
        slot.queued += 1;
        slot.pending = Some(state);
        self.inner.wake.notify_all();
    }

    /// Wait until everything asked for so far has been written. For tests, and
    /// for the tidy path out of `main`.
    pub fn flush(&self) {
        if self.inner.path.is_none() {
            return;
        }
        let mut slot = self.inner.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let want = slot.queued;
        while !slot.stop && slot.done < want {
            let (g, t) = self
                .inner
                .done
                .wait_timeout(slot, std::time::Duration::from_secs(5))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            slot = g;
            if t.timed_out() {
                break;
            }
        }
    }

    #[must_use]
    pub fn health(&self) -> StoreHealth {
        self.inner.health.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Where the state file is, for the status route.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.inner.path.as_deref()
    }

    /// Stop the writer thread, after finishing what it was asked for.
    pub fn stop(&self) {
        {
            let mut slot = self.inner.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if slot.stop {
                return;
            }
            slot.stop = true;
        }
        self.inner.wake.notify_all();
        self.inner.done.notify_all();
        if let Some(h) = self.writer.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
            let _ = h.join();
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        self.stop();
    }
}

fn write_loop(inner: &Arc<Inner>) {
    let Some(path) = inner.path.clone() else { return };
    let mut written: Option<String> = None;
    loop {
        let (next, upto) = {
            let mut slot = inner.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            while slot.pending.is_none() && !slot.stop {
                slot = inner.wake.wait(slot).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            let upto = slot.queued;
            match slot.pending.take() {
                Some(p) => (p, upto),
                // Stopping, and nothing left to write.
                None => break,
            }
        };

        let text = match serde_json::to_string_pretty(&next) {
            Ok(mut t) => {
                t.push('\n');
                t
            }
            Err(e) => {
                note_error(inner, format!("serialising the state: {e}"));
                finish_pass(inner, upto);
                continue;
            }
        };
        // Nothing changed: do not touch the disk. A slider being dragged makes
        // one state change per frame and most of them are identical by the
        // time the writer gets here.
        if written.as_deref() == Some(text.as_str()) {
            finish_pass(inner, upto);
            continue;
        }
        match write_atomically(&path, text.as_bytes()) {
            Ok(()) => {
                written = Some(text);
                let mut h = inner.health.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                h.writes += 1;
                h.last_write_unix = Some(unix_now());
                h.last_error = None;
            }
            Err(e) => note_error(inner, format!("writing {}: {e}", path.display())),
        }
        finish_pass(inner, upto);
    }
    // Last wishes: whatever was asked for on the way out.
    let (last, upto) = {
        let mut slot = inner.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        (slot.pending.take(), slot.queued)
    };
    if let Some(p) = last {
        if let Ok(t) = serde_json::to_string_pretty(&p) {
            let _ = write_atomically(&path, format!("{t}\n").as_bytes());
        }
    }
    finish_pass(inner, upto);
}

/// Everything asked for up to `upto` has now been dealt with.
fn finish_pass(inner: &Arc<Inner>, upto: u64) {
    let mut slot = inner.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    slot.done = slot.done.max(upto);
    inner.done.notify_all();
}

/// Logged once per distinct message, not per attempt: a disk that has gone
/// read-only must not fill the log at one line a second for a month.
fn note_error(inner: &Arc<Inner>, msg: String) {
    let mut h = inner.health.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if h.last_error.as_deref() != Some(msg.as_str()) {
        eprintln!("studio: state: {msg}");
    }
    h.last_error = Some(msg);
}

/// Temp file, `fsync`, rename. The rename is atomic on every filesystem this
/// runs on, so a reader sees the whole old file or the whole new one.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => {}
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    }
    // Best effort: the directory entry itself. Not fatal if the platform will
    // not open a directory for this.
    if let Some(dir) = path.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

#[must_use]
pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of our own, removed when the test ends.
    struct Temp(PathBuf);
    impl Temp {
        fn new(tag: &str) -> Temp {
            let p = std::env::temp_dir().join(format!("screeny-studio-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).expect("a temp dir");
            Temp(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_store_with_no_directory_keeps_everything_in_memory() {
        let (store, loaded) = Store::open(None);
        assert_eq!(loaded, Persisted::default());
        store.save(Persisted::default());
        store.flush();
        let h = store.health();
        assert!(!h.persisting);
        assert_eq!(h.writes, 0);
        assert_eq!(h.path, "(memory)");
    }

    #[test]
    fn a_save_round_trips_through_the_file() {
        let dir = Temp::new("roundtrip");
        let (store, loaded) = Store::open(Some(&dir.0));
        assert_eq!(loaded.version, SCHEMA_VERSION);
        assert!(loaded.devices.is_empty());

        let mut want = Persisted::default();
        want.devices.push(StoredDevice { id: "abc123".into(), name: "desk".into(), ..StoredDevice::default() });
        want.players.push(StoredPlayer { device: "abc123".into(), patch: "metaballs".into(), seed: 7, ..StoredPlayer::default() });
        want.focus = "abc123".into();
        store.save(want.clone());
        store.flush();
        assert_eq!(store.health().writes, 1);

        // A second identical save costs nothing: the file is not rewritten.
        store.save(want.clone());
        store.flush();
        assert_eq!(store.health().writes, 1, "an unchanged state should not touch the disk");

        store.stop();
        let (_store2, back) = Store::open(Some(&dir.0));
        assert_eq!(back, want);
    }

    /// The four ways a state file can be unusable. None of them may stop the
    /// server, and none of them may throw the file away.
    #[test]
    fn a_state_file_that_cannot_be_used_starts_a_sane_default() {
        for (tag, content) in [
            ("corrupt", "{not json at all"),
            ("truncated", r#"{"version":1,"devices":[{"id":"ab"#),
            ("wrongtype", r#"{"version":1,"devices":"not a list"}"#),
            ("empty", ""),
        ] {
            let dir = Temp::new(tag);
            std::fs::write(dir.0.join(FILE), content).expect("write the bad file");
            let (store, loaded) = Store::open(Some(&dir.0));
            assert_eq!(loaded, Persisted::default(), "{tag} should start from defaults");
            let h = store.health();
            assert!(h.recovered.is_some(), "{tag} should say why it started from defaults");
            assert!(dir.0.join(BAD_FILE).is_file(), "{tag}: the unreadable file should be kept, not deleted");
            assert!(!dir.0.join(FILE).exists(), "{tag}: the unreadable file should be out of the way");

            // And the server carries on: the next save works.
            store.save(loaded);
            store.flush();
            assert_eq!(store.health().writes, 1, "{tag}: the store should still write");
            assert!(store.health().last_error.is_none());
        }
    }

    #[test]
    fn a_state_file_from_the_future_is_kept_not_parsed() {
        let dir = Temp::new("future");
        let future = format!(r#"{{"version":{},"devices":[],"players":[],"whatever":true}}"#, SCHEMA_VERSION + 9);
        std::fs::write(dir.0.join(FILE), &future).expect("write the future file");
        let (store, loaded) = Store::open(Some(&dir.0));
        assert_eq!(loaded, Persisted::default());
        let why = store.health().recovered.expect("a reason");
        assert!(why.contains(&format!("v{}", SCHEMA_VERSION + 9)), "{why}");
        let kept = dir.0.join(format!("state.v{}.json", SCHEMA_VERSION + 9));
        assert_eq!(std::fs::read_to_string(&kept).expect("kept"), future);
    }

    #[test]
    fn a_file_with_no_version_is_migrated_rather_than_thrown_away() {
        let dir = Temp::new("migrate");
        std::fs::write(dir.0.join(FILE), r#"{"players":[{"device":"abc","piece":"metaballs","seed":3}]}"#).expect("write");
        let (store, loaded) = Store::open(Some(&dir.0));
        assert_eq!(loaded.version, SCHEMA_VERSION);
        assert_eq!(loaded.players.len(), 1);
        assert_eq!(loaded.players[0].seed, 3);
        // Fields the old file did not have take their defaults, not zeroes.
        assert!(loaded.players[0].on);
        assert_eq!(loaded.players[0].speed, 1.0, "v3's new fields take their defaults");
        assert!(!loaded.players[0].paused);
        assert_eq!(loaded.focus, "abc", "and the page looks at the one player there is");
        assert!(store.health().recovered.is_some());
    }

    /// A missing file is the first run, not a fault: nothing is reported and
    /// nothing is moved aside.
    #[test]
    fn a_missing_file_is_not_a_problem() {
        let dir = Temp::new("missing");
        let (store, loaded) = Store::open(Some(&dir.0));
        assert_eq!(loaded, Persisted::default());
        assert!(store.health().recovered.is_none());
        assert!(store.health().last_error.is_none());
    }

    // ------------------------- the per-patch memory (card 165) -------------------------

    /// The patch these tests tune. It was `plasma` until card 178 removed it;
    /// `metaballs` is the replacement, and what is needed of it is only that
    /// it is in every build and has parameters with ranges - `hue` (0..360) is
    /// the one moved about below, chosen because every value these tests use
    /// sits inside its range and none of them is its default, so a clamp or a
    /// "that is already the default" is never an accident.
    fn metaballs() -> &'static PatchDef {
        screeny_art::patch::find("metaballs").expect("metaballs is in every build")
    }

    fn spec(def: &PatchDef, id: &str) -> ParamSpec {
        *def.params.iter().find(|s| s.id == id).expect("a parameter of this patch")
    }

    /// The decision the card is built on: only what a human actually moved is
    /// written down, so a later release's better default reaches everybody who
    /// never touched that slider.
    #[test]
    fn only_what_differs_from_the_defaults_is_remembered() {
        let def = metaballs();
        let mut memory = Memory::new();
        let mut live: BTreeMap<String, f32> = def.params.iter().map(|s| (s.id.to_string(), s.default)).collect();
        remember(&mut memory, def, &live, 7, 1.0);
        assert_eq!(memory["metaballs"].params, BTreeMap::new(), "untouched defaults are not worth a byte");
        assert_eq!(memory["metaballs"].seed, Some(7));

        live.insert("hue".into(), 2.5);
        remember(&mut memory, def, &live, 7, 1.0);
        assert_eq!(memory["metaballs"].params.len(), 1);
        assert_eq!(memory["metaballs"].params["hue"], 2.5);

        // And putting it back where it started forgets it again.
        live.insert("hue".into(), spec(def, "hue").default);
        remember(&mut memory, def, &live, 7, 1.0);
        assert!(memory["metaballs"].params.is_empty());

        // Card 151: speed is part of the working copy now, for the same reason
        // it is part of a setting.
        remember(&mut memory, def, &live, 7, 0.4);
        assert_eq!(memory["metaballs"].speed, Some(0.4));
    }

    /// Reset means "back to the defaults and *stay* there", so the parameters
    /// are forgotten. The seed is not: the seed is not what Reset is about, and
    /// throwing it away would make a switch back rebuild the patch on whatever
    /// seed the *other* patch happened to be on.
    #[test]
    fn reset_forgets_the_parameters_and_keeps_the_seed() {
        let def = metaballs();
        let mut memory = Memory::new();
        remember(&mut memory, def, &BTreeMap::from([("hue".to_string(), 2.5)]), 99, 1.0);
        forget_params(&mut memory, "metaballs");
        assert_eq!(memory["metaballs"].seed, Some(99));
        assert!(memory["metaballs"].params.is_empty());

        // An entry with nothing left in it at all goes away entirely.
        memory.insert(
            "ghost".into(),
            PatchMemory { params: BTreeMap::from([("x".into(), 1.0)]), ..PatchMemory::default() },
        );
        forget_params(&mut memory, "ghost");
        assert!(!memory.contains_key("ghost"));
    }

    /// Every error case the card lists, one value at a time, and the rule that
    /// matters: one bad value never costs the others.
    #[test]
    fn a_value_this_build_cannot_use_becomes_the_default_and_the_rest_survive() {
        let def = metaballs();
        let hue = spec(def, "hue");
        let remembered = BTreeMap::from([
            ("hue".to_string(), 2.5),                // good, and must survive all of this
            ("gone".to_string(), 1.0),               // a parameter this build does not have
            ("speed".to_string(), 99.0),             // above the range
            ("spread".to_string(), -99.0),           // below the range
            ("count".to_string(), f32::NAN),         // not a number
            ("samples".to_string(), f32::INFINITY),  // not a number either
            ("size".to_string(), spec(def, "size").default), // already the default
        ]);
        let (usable, repaired) = usable_params(&remembered, def.params);

        assert_eq!(usable["hue"], 2.5, "the good value survived every bad one");
        assert!(!usable.contains_key("gone"), "an unknown parameter is ignored");
        assert_eq!(usable["speed"], spec(def, "speed").max, "out of range is clamped, as the slider would");
        assert_eq!(usable["spread"], spec(def, "spread").min);
        assert!(!usable.contains_key("count"), "a NaN falls back to the default");
        assert!(!usable.contains_key("samples"), "an infinity falls back to the default");
        assert!(!usable.contains_key("size"), "a value that is already the default is not stored");
        assert_eq!(repaired.len(), 5, "one sentence per correction: {repaired:?}");
        assert_eq!(hue.sanitise(2.5), 2.5);
    }

    /// The correction is written back, so it happens once rather than on every
    /// switch - which is what makes "logged once" true without keeping a set of
    /// things already said.
    #[test]
    fn a_correction_is_made_once_and_written_back() {
        let def = metaballs();
        let mut memory = Memory::new();
        memory.insert(
            "metaballs".into(),
            PatchMemory {
                seed: Some(5),
                params: BTreeMap::from([("hue".into(), 999.0), ("gone".into(), 1.0)]),
                ..PatchMemory::default()
            },
        );
        let first = recall(&mut memory, def, "a test");
        assert_eq!(first.seed, Some(5));
        assert_eq!(first.params["hue"], spec(def, "hue").max);
        assert_eq!(memory["metaballs"].params, first.params, "the file's copy was corrected too");
        // Second time round there is nothing left to correct, so nothing to say.
        let again = recall(&mut memory, def, "a test");
        assert_eq!(again, first);
    }

    /// The whole point: switch away, switch back, and it is as you left it.
    ///
    /// The tuned entry here is **`plasma`, which this build has not got** -
    /// card 178 removed it - and that is deliberate: the owner may have spent
    /// an evening on it, a patch can come back, and the promise is that a
    /// memory entry for a patch nobody can play any more round-trips through
    /// the file untouched, settings and all. `back == want` is that promise.
    #[test]
    fn a_memory_round_trips_through_the_file() {
        let dir = Temp::new("memory-roundtrip");
        let (store, _) = Store::open(Some(&dir.0));
        let mut want = Persisted::default();
        want.patches.insert(
            "plasma".into(),
            PatchMemory {
                seed: Some(3),
                params: BTreeMap::from([("scale".into(), 2.5)]),
                speed: Some(0.4),
                setting: "Lava".into(),
                settings: BTreeMap::from([(
                    "Lava".to_string(),
                    Setting { seed: 3, params: BTreeMap::from([("scale".into(), 2.5)]), speed: 0.4 },
                )]),
            },
        );
        want.patches.insert(
            "metaballs".into(),
            PatchMemory { seed: Some(9), params: BTreeMap::from([("count".into(), 8.0)]), ..PatchMemory::default() },
        );
        want.players.push(StoredPlayer { device: "abc".into(), ..StoredPlayer::default() });
        store.save(want.clone());
        store.flush();
        store.stop();

        let text = std::fs::read_to_string(dir.0.join(FILE)).expect("read");
        assert!(text.contains("\"patches\""), "the memory is in the state file, not somewhere else:\n{text}");
        let (_s2, back) = Store::open(Some(&dir.0));
        assert_eq!(back, want);
        assert_eq!(back.version, SCHEMA_VERSION);
    }

    /// A v1 file as the deployed service has one today (the shape is card 106's
    /// Log). What each context was playing becomes that patch's first memory:
    /// nobody loses the tuning they have.
    #[test]
    fn a_real_v1_file_migrates_without_losing_anything() {
        let dir = Temp::new("v1");
        let v1 = r#"{
  "version": 1,
  "devices": [
    { "id": "4a00a4", "name": "desk", "instance": "screeny-4a00a4", "address": "", "manual": false }
  ],
  "players": [
    {
      "device": "4a00a4",
      "on": true,
      "piece": "metaballs",
      "seed": 4242,
      "params": {
        "count": 5.0, "speed": 0.5, "size": 2.5, "hue": 20.0, "spread": 200.0, "samples": 4.0
      },
      "fps": 30.0,
      "settings": {
        "levels": 64, "dither": "bayer4",
        "limiter": { "enabled": true, "apl_cap": 0.4, "max_rise_per_s": 2.0 },
        "panel_model": true, "codec_preview": true
      },
      "brightness": 96
    }
  ],
  "preview": {
    "piece": "clocks-numerals",
    "seed": 7,
    "params": { "rest": 3.0 },
    "settings": {
      "levels": 64, "dither": "bayer4",
      "limiter": { "enabled": true, "apl_cap": 0.4, "max_rise_per_s": 2.0 },
      "panel_model": true, "codec_preview": true
    },
    "paused": false, "speed": 1.0, "fps": 60.0,
    "panel_on": true, "panel_to": "screeny-4a00a4"
  }
}"#;
        std::fs::write(dir.0.join(FILE), v1).expect("write the v1 file");
        let (store, loaded) = Store::open(Some(&dir.0));

        // Nothing v1 knew about is lost.
        assert_eq!(loaded.version, SCHEMA_VERSION);
        assert_eq!(loaded.devices.len(), 1);
        assert_eq!(loaded.devices[0].id, "4a00a4");
        assert_eq!(loaded.players[0].patch, "metaballs");
        assert_eq!(loaded.players[0].seed, 4242);
        assert_eq!(loaded.players[0].brightness, Some(96));
        assert_eq!(loaded.players.len(), 1, "the panel's player is the truth; the preview block is not a second one");
        assert_eq!(loaded.focus, "4a00a4", "and it is what the page looks at");

        // And what each context was playing is merged into the one memory -
        // only the values that were actually moved, not the whole v1 dump.
        let m = &loaded.patches;
        assert_eq!(m["metaballs"].seed, Some(4242), "what the panel was playing");
        assert_eq!(m["metaballs"].params, BTreeMap::from([("size".to_string(), 2.5)]), "only `size` was off its default");
        assert_eq!(m["clocks-numerals"].seed, Some(7), "and what the design view was showing");
        assert_eq!(m["clocks-numerals"].params, BTreeMap::from([("rest".to_string(), 3.0)]));

        assert!(store.health().recovered.is_some_and(|w| w.contains("v1")), "the migration says so once");
        assert_eq!(loaded.version, SCHEMA_VERSION);
        // The one thing a good v1 file now needs said about it: every one of
        // them names `settings.levels`, and card 102 retired it.
        // ... and, since card 161, its `fps`.
        let said = store.health().repaired;
        assert_eq!(said.len(), 2, "a good v1 file needs no other repair: {said:?}");
        assert!(said[0].contains("`settings.levels` (64)"), "{}", said[0]);
        assert!(said[0].contains("`output.panel`"), "and says what replaced it: {}", said[0]);
        assert!(said[1].starts_with("`fps`"), "and the retired rate: {}", said[1]);
        assert_eq!(
            loaded.players[0].output.panel,
            screeny_art::panel::Panel::Dithered,
            "and the panel comes up at what the device really shows"
        );
        assert!(!dir.0.join(BAD_FILE).exists(), "a v1 file is migrated, not condemned");
    }

    /// The one rule the merge needs: when a v1 file's design view and a panel
    /// were on the *same* patch with different values, the panel's win. The
    /// panel is what was actually being looked at.
    #[test]
    fn a_v1_merge_prefers_what_the_panel_was_playing() {
        let dir = Temp::new("v1-conflict");
        std::fs::write(
            dir.0.join(FILE),
            r#"{
              "version": 1,
              "players": [ { "device": "4a00a4", "piece": "metaballs", "seed": 2, "params": { "size": 2.5 } } ],
              "preview":   { "piece": "metaballs", "seed": 1, "params": { "size": 2.0 } }
            }"#,
        )
        .expect("write");
        let (_store, loaded) = Store::open(Some(&dir.0));
        assert_eq!(loaded.patches["metaballs"].params["size"], 2.5, "the panel's value");
        assert_eq!(loaded.patches["metaballs"].seed, Some(2), "and the panel's seed");
    }

    // ------------------------- v2 -> v3 (card 170) -------------------------

    /// A **real v2 file**: the shape the live service has today, from card
    /// 165's Log - one device, one player, a `preview` block and the output
    /// memory. Card 170 drops `preview`; nothing else may move.
    const LIVE_V2: &str = r#"{
  "version": 2,
  "devices": [
    { "id": "4a00a4", "name": "", "instance": "screeny-4a00a4", "address": "", "manual": false }
  ],
  "players": [
    {
      "device": "4a00a4",
      "on": true,
      "piece": "overland",
      "seed": 4242,
      "params": {},
      "fps": 30.0,
      "settings": {
        "levels": 64, "dither": "bayer4",
        "limiter": { "enabled": true, "apl_cap": 0.4, "max_rise_per_s": 2.0 },
        "panel_model": true, "codec_preview": true
      },
      "brightness": null
    }
  ],
  "preview": {
    "piece": "clocks-numerals",
    "seed": 7,
    "params": { "rest": 3.0 },
    "settings": {
      "levels": 64, "dither": "bayer4",
      "limiter": { "enabled": true, "apl_cap": 0.4, "max_rise_per_s": 2.0 },
      "panel_model": true, "codec_preview": true
    },
    "paused": false, "speed": 1.0, "fps": 60.0,
    "panel_on": false, "panel_to": ""
  },
  "pieces": {
    "clocks-numerals": { "seed": 0 },
    "metaballs": { "seed": 0, "params": { "count": 8.0 } },
    "overland": { "seed": 4242 },
    "plasma": { "seed": 0, "params": { "scale": 2.97 } }
  }
}"#;

    /// The panel's player is the truth, the memory is not disturbed, and the
    /// `preview` block leaves without taking anything with it.
    #[test]
    fn a_real_v2_file_migrates_to_v3() {
        let dir = Temp::new("v2");
        std::fs::write(dir.0.join(FILE), LIVE_V2).expect("write the v2 file");
        let (store, loaded) = Store::open(Some(&dir.0));

        assert_eq!(loaded.version, SCHEMA_VERSION);
        assert_eq!(loaded.devices.len(), 1);
        assert_eq!(loaded.devices[0].id, "4a00a4");

        // The player, untouched, plus v3's two new fields at their defaults.
        assert_eq!(loaded.players.len(), 1, "the preview block must not become a second player");
        let p = &loaded.players[0];
        assert_eq!(p.device, "4a00a4");
        assert_eq!(p.patch, "overland");
        assert_eq!(p.seed, 4242);
        assert!(p.on);
        assert!(!p.paused);
        assert_eq!(p.speed, 1.0);
        assert_eq!(loaded.focus, "4a00a4", "the page is a window onto the panel");

        // The memory is v2's, exactly: the preview's patch was already in it,
        // so its values must not have been written over the top.
        assert_eq!(loaded.patches.len(), 4);
        // `plasma` went with card 178, so this entry is now for a patch this
        // build has not got - and it is still here, which is the promise.
        assert_eq!(loaded.patches["plasma"].params["scale"], 2.97);
        assert_eq!(loaded.patches["metaballs"].params["count"], 8.0);
        assert_eq!(loaded.patches["overland"].seed, Some(4242));
        assert_eq!(loaded.patches["clocks-numerals"].seed, Some(0), "the v2 entry wins over the preview block's");
        assert!(loaded.patches["clocks-numerals"].params.is_empty(), "including its parameters");

        assert!(store.health().recovered.is_some_and(|w| w.contains("v2")), "it says so once");
        let said = store.health().repaired;
        assert_eq!(said.len(), 2, "the retired `levels` (card 102) and `fps` (card 161): {said:?}");
        assert!(said[0].contains("`settings.levels`"), "{}", said[0]);
        assert!(said[1].starts_with("`fps`"), "{}", said[1]);
        assert!(!dir.0.join(BAD_FILE).exists(), "a v2 file is migrated, not condemned");
    }

    /// Card 102. A current file that still names the retired `levels` - 32 or
    /// 16, the panel models that stood for a dimming this device has never done -
    /// loads, keeps everything else it says, comes up on the device's own
    /// panel model, and is told about **once**. No schema bump: the shape of
    /// the file did not change, only what one key means.
    #[test]
    fn a_retired_levels_setting_loads_and_is_reported() {
        for level in ["32", "16", "64"] {
            let dir = Temp::new(&format!("levels-{level}"));
            std::fs::write(
                dir.0.join(FILE),
                format!(
                    r#"{{"version":{SCHEMA_VERSION},"players":[{{"device":"","piece":"metaballs","seed":9,
                       "settings":{{"levels":{level},"dither":"bayer8","panel_model":false}}}}]}}"#
                ),
            )
            .expect("write the file");
            let (store, loaded) = Store::open(Some(&dir.0));

            assert_eq!(loaded.players[0].seed, 9, "levels {level}: the rest of the file survives");
            assert_eq!(loaded.players[0].output.dither, screeny_art::dither::Dither::Bayer8);
            assert!(!loaded.players[0].output.panel_model, "levels {level}: and the rest of the output block");
            assert_eq!(
                loaded.players[0].output.panel,
                screeny_art::panel::Panel::Dithered,
                "levels {level}: the panel model is the device's"
            );

            let said = store.health().repaired;
            assert_eq!(said.len(), 1, "levels {level}: said once, not per player: {said:?}");
            assert!(said[0].contains(&format!("`settings.levels` ({level})")), "{}", said[0]);
            assert!(store.health().recovered.is_none(), "levels {level}: not a recovery, the file was used");
            assert!(!dir.0.join(BAD_FILE).exists(), "levels {level}: and certainly not condemned");
        }
    }

    /// Card 161. A current (v5) file that still names `fps` - 60, or any of
    /// the rates card 172's slider could reach - loads, keeps everything else
    /// it says, plays at the one rate, and is told about **once**. No schema
    /// bump, for card 102's reason: the shape of the file did not change, only
    /// that one key has stopped meaning anything.
    #[test]
    fn a_retired_fps_loads_and_is_reported() {
        for rate in ["60.0", "10.0", "30.0"] {
            let dir = Temp::new(&format!("fps-{rate}"));
            std::fs::write(
                dir.0.join(FILE),
                format!(
                    r#"{{"version":{SCHEMA_VERSION},"focus":"","players":[{{"device":"","patch":"metaballs","seed":9,
                       "fps":{rate},"paused":true,"speed":0.5,
                       "output":{{"dither":"bayer8","panel_model":false}}}}]}}"#
                ),
            )
            .expect("write the file");
            let (store, loaded) = Store::open(Some(&dir.0));

            assert_eq!(loaded.players.len(), 1, "fps {rate}: the file was used");
            assert_eq!(loaded.players[0].seed, 9, "fps {rate}: the rest of the file survives");
            assert!(loaded.players[0].paused, "fps {rate}: including the playback state that is still a thing");
            assert_eq!(loaded.players[0].speed, 0.5);
            assert_eq!(loaded.players[0].output.dither, screeny_art::dither::Dither::Bayer8);

            let said = store.health().repaired;
            assert_eq!(said.len(), 1, "fps {rate}: said once, not per player: {said:?}");
            assert!(said[0].contains(&format!("`fps` ({rate})")), "{}", said[0]);
            assert!(
                said[0].contains(&format!("{} fps", screeny_art::FPS)),
                "and says what there is instead: {}",
                said[0]
            );
            assert!(store.health().recovered.is_none(), "fps {rate}: not a recovery - the file is v{SCHEMA_VERSION}");
            assert!(!dir.0.join(BAD_FILE).exists(), "fps {rate}: and certainly not condemned");
            assert!(
                !dir.0.join(format!("state.v{SCHEMA_VERSION}.json")).exists(),
                "fps {rate}: no schema bump means no migration and no backup copy"
            );
        }
    }

    /// Two players that both name the retired key are still **one** sentence,
    /// in the `repaired` style: it is about the key, not about each player.
    #[test]
    fn a_retired_fps_is_said_once_for_the_whole_file() {
        let dir = Temp::new("fps-many");
        std::fs::write(
            dir.0.join(FILE),
            format!(
                r#"{{"version":{SCHEMA_VERSION},"focus":"a","players":[
                   {{"device":"a","patch":"metaballs","fps":60.0}},
                   {{"device":"b","patch":"clocks-dials","fps":60.0}}]}}"#
            ),
        )
        .expect("write the file");
        let (store, loaded) = Store::open(Some(&dir.0));
        assert_eq!(loaded.players.len(), 2);
        let said = store.health().repaired;
        assert_eq!(said.len(), 1, "one sentence about the key, not one per player: {said:?}");
    }

    /// The design view's patch is only merged into the memory where the memory
    /// knows nothing about it - a v2 file whose preview was on a patch no
    /// player had ever touched.
    #[test]
    fn a_v2_preview_on_an_unknown_patch_is_kept() {
        let dir = Temp::new("v2-gap");
        std::fs::write(
            dir.0.join(FILE),
            r#"{
              "version": 2,
              "players": [ { "device": "4a00a4", "piece": "clocks-dials", "seed": 1 } ],
              "preview": { "piece": "metaballs", "seed": 9, "params": { "count": 8.0 } },
              "pieces": { "clocks-dials": { "seed": 1 } }
            }"#,
        )
        .expect("write");
        let (_store, loaded) = Store::open(Some(&dir.0));
        assert_eq!(loaded.patches["metaballs"].seed, Some(9), "nothing knew about it, so it is kept");
        assert_eq!(loaded.patches["metaballs"].params["count"], 8.0);
        assert_eq!(loaded.players.len(), 1, "and it is still not a player");
    }

    /// The one case where dropping `panel_on` would lose something: a v2 file
    /// with **no** players, where the design view was the only thing playing.
    /// It becomes the player, on the device it was pointed at.
    #[test]
    fn a_v2_preview_that_was_the_only_thing_playing_becomes_the_player() {
        let dir = Temp::new("v2-onlypreview");
        std::fs::write(
            dir.0.join(FILE),
            r#"{
              "version": 2,
              "devices": [ { "id": "4a00a4", "instance": "screeny-4a00a4" } ],
              "players": [],
              "preview": { "piece": "metaballs", "seed": 55, "params": { "size": 2.5 },
                           "paused": true, "speed": 2.0, "fps": 30.0,
                           "panel_on": true, "panel_to": "screeny-4a00a4" }
            }"#,
        )
        .expect("write");
        let (_store, loaded) = Store::open(Some(&dir.0));
        assert_eq!(loaded.players.len(), 1);
        let p = &loaded.players[0];
        assert_eq!(p.device, "4a00a4", "`panel_to` named it by instance name; the id is what a player uses");
        assert!(p.on, "it was streaming, so it keeps streaming");
        assert_eq!(p.patch, "metaballs");
        assert_eq!(p.seed, 55);
        assert_eq!(p.params["size"], 2.5);
        assert!(p.paused);
        assert_eq!(p.speed, 2.0);
        assert_eq!(loaded.focus, "4a00a4");
    }

    /// The same, with nothing to point at: the player is unbound rather than
    /// aimed at a device the file has never heard of.
    #[test]
    fn a_v2_preview_with_no_panel_becomes_an_unbound_player() {
        let dir = Temp::new("v2-unbound");
        std::fs::write(
            dir.0.join(FILE),
            r#"{"version":2,"players":[],"preview":{"piece":"metaballs","seed":3,"panel_on":false}}"#,
        )
        .expect("write");
        let (_store, loaded) = Store::open(Some(&dir.0));
        assert_eq!(loaded.players.len(), 1);
        assert_eq!(loaded.players[0].device, UNBOUND);
        assert_eq!(loaded.players[0].patch, "metaballs");
        assert!(!loaded.players[0].on, "nothing to send to");
        assert_eq!(loaded.focus, UNBOUND);
    }

    /// A hand-edited file full of rubbish in the memory. Every one of these is
    /// a *value* problem, and card 106's rule is that a value problem must not
    /// cost the file: nothing here may reach `state.bad.json`.
    #[test]
    fn rubbish_in_the_memory_costs_exactly_the_rubbish() {
        let dir = Temp::new("rubbish");
        let file = format!(
            r#"{{
  "version": {SCHEMA_VERSION},
  "preview": {{ "piece": "metaballs" }},
  "pieces": {{
    "metaballs": {{ "seed": 11, "params": {{ "hue": 2.5, "speed": null, "spread": "fast", "count": {{}}, "samples": [] }} }},
    "clocks-numerals": {{ "seed": "not a seed", "params": {{ "rest": 3.0 }} }},
    "no-such-patch": {{ "seed": 4, "params": {{ "whatever": 1.0 }} }},
    "vesta": "not an object at all",
    "clocks-dials": {{ "params": "not an object either" }}
  }}
}}"#
        );
        std::fs::write(dir.0.join(FILE), &file).expect("write");
        let (store, loaded) = Store::open(Some(&dir.0));

        assert!(!dir.0.join(BAD_FILE).exists(), "a bad value must never condemn the file");
        let m = &loaded.patches;
        assert_eq!(m["metaballs"].seed, Some(11));
        assert_eq!(m["metaballs"].params, BTreeMap::from([("hue".to_string(), 2.5)]), "one good value, four bad ones dropped");
        assert_eq!(m["clocks-numerals"].seed, None, "a seed that is not a seed is forgotten");
        assert_eq!(m["clocks-numerals"].params["rest"], 3.0, "and the rest of that patch's memory survives it");
        assert!(m.contains_key("no-such-patch"), "an unknown patch keeps its entry, so a patch that comes back gets it");
        assert!(!m.contains_key("vesta"), "an entry that is not an object is forgotten");
        assert!(!m.contains_key("clocks-dials"), "so is one with nothing usable left in it");
        assert!(!store.health().repaired.is_empty(), "and the server says what it had to correct");
        assert!(store.health().last_error.is_none());
    }

    /// **Card 178.** A file whose player is on a patch this build has not got
    /// (`plasma`, which this is the first build without) loads, plays the
    /// default patch, and says so **once**, in `repaired`.
    ///
    /// The three things asserted beyond that are the ones that make it safe to
    /// do at all: the file is not a recovery (it was read and used), the
    /// memory entry for the missing patch is **still there**, and the default
    /// patch arrives set the way the file says it was left, not on the missing
    /// patch's seed.
    #[test]
    fn a_player_on_a_patch_that_is_gone_plays_the_default_and_says_so() {
        let dir = Temp::new("gone-patch");
        std::fs::write(
            dir.0.join(FILE),
            format!(
                r#"{{"version":{SCHEMA_VERSION},"focus":"abc",
                     "players":[{{"device":"abc","patch":"plasma","seed":4242,"params":{{"scale":2.5}},"speed":0.5}}],
                     "patches":{{"plasma":{{"seed":4242,"params":{{"scale":2.5}}}},
                                 "{}":{{"seed":77,"params":{{"rest":3.0}},"speed":0.25}}}}}}"#,
                default_patch()
            ),
        )
        .expect("write");
        let (store, loaded) = Store::open(Some(&dir.0));

        assert_eq!(loaded.players.len(), 1, "the file was used, not thrown away");
        assert_eq!(loaded.players[0].patch, default_patch(), "a player must always have something to play");
        assert_eq!(loaded.players[0].device, "abc", "and it is still that panel's player");
        // Not plasma's 4242 and not plasma's 0.5x: the default patch arrives
        // as the studio last left *it*, which is what switching to it by hand
        // would do.
        assert_eq!(loaded.players[0].seed, 77, "the default patch's own remembered seed");
        assert_eq!(loaded.players[0].speed, 0.25, "and its own speed");
        assert_eq!(loaded.players[0].params["rest"], 3.0, "and its own parameters");
        assert!(!loaded.players[0].params.contains_key("scale"), "never the missing patch's: {:?}", loaded.players[0].params);

        // The memory is untouched. Somebody may have spent an evening on it,
        // and a patch can come back.
        assert_eq!(loaded.patches["plasma"].seed, Some(4242), "the tuning is kept, inert");
        assert_eq!(loaded.patches["plasma"].params["scale"], 2.5);

        let h = store.health();
        assert!(h.recovered.is_none(), "the file was read and used; this is not a recovery: {:?}", h.recovered);
        assert!(!dir.0.join(BAD_FILE).exists(), "and certainly not condemned");
        let said: Vec<&String> = h.repaired.iter().filter(|s| s.contains("plasma")).collect();
        assert_eq!(said.len(), 1, "said once: {:?}", h.repaired);
        assert!(said[0].contains(default_patch()), "and says what is playing instead: {}", said[0]);
    }

    /// Once **per player**, because it is a fact about each panel rather than
    /// about the file - unlike the retired `levels` and `fps` keys, which are
    /// about the file and are said once for all of it.
    #[test]
    fn two_players_on_a_patch_that_is_gone_are_two_sentences() {
        let dir = Temp::new("gone-patch-two");
        std::fs::write(
            dir.0.join(FILE),
            format!(
                r#"{{"version":{SCHEMA_VERSION},"focus":"a","players":[
                     {{"device":"a","patch":"plasma"}},
                     {{"device":"b","patch":"testcard"}},
                     {{"device":"c","patch":"metaballs"}}]}}"#
            ),
        )
        .expect("write");
        let (store, loaded) = Store::open(Some(&dir.0));
        assert_eq!(loaded.players.len(), 3);
        assert_eq!(loaded.players[0].patch, default_patch());
        assert_eq!(loaded.players[1].patch, default_patch());
        assert_eq!(loaded.players[2].patch, "metaballs", "a panel on a patch that is still here is not disturbed");
        let said = store.health().repaired;
        assert_eq!(said.len(), 2, "one per panel that had to be moved: {said:?}");
        assert!(said[0].contains("plasma") && said[0].contains("panel a"), "{}", said[0]);
        assert!(said[1].contains("testcard") && said[1].contains("panel b"), "{}", said[1]);
    }

    /// And through a migration: a v1 file whose only player was the design
    /// view, on a patch that has since gone. The v1 -> v3 step builds that
    /// player out of the `preview` block, so the repair has to run after it.
    #[test]
    fn a_v1_file_on_a_patch_that_is_gone_still_comes_up_playing() {
        let dir = Temp::new("gone-patch-v1");
        std::fs::write(
            dir.0.join(FILE),
            r#"{"version":1,"players":[],"preview":{"piece":"plasma","seed":55,"panel_on":false}}"#,
        )
        .expect("write");
        let (store, loaded) = Store::open(Some(&dir.0));
        assert_eq!(loaded.players.len(), 1, "a studio always has a picture to show");
        assert_eq!(loaded.players[0].patch, default_patch());
        assert_eq!(loaded.players[0].device, UNBOUND);
        assert!(store.health().repaired.iter().any(|s| s.contains("plasma")), "{:?}", store.health().repaired);
        // The v1 merge wrote nothing for it - `migrate_to_v3` skips a patch it
        // does not know - so there is nothing to keep and nothing is invented.
        assert!(!loaded.patches.contains_key("plasma"), "{:?}", loaded.patches);
    }

    /// The bound. The studio only ever writes a memory for a patch it can play,
    /// so this is the other direction: a hand-edited file cannot make the state
    /// file grow for ever.
    #[test]
    fn unknown_patches_are_capped() {
        let dir = Temp::new("cap");
        let entries: Vec<String> =
            (0..MAX_UNKNOWN_PATCHES + 20).map(|i| format!(r#""ghost-{i:03}": {{"seed": {i}}}"#)).collect();
        let file = format!(
            r#"{{"version":{SCHEMA_VERSION},"preview":{{"piece":"metaballs"}},"pieces":{{{},"metaballs":{{"seed":1}}}}}}"#,
            entries.join(",")
        );
        std::fs::write(dir.0.join(FILE), &file).expect("write");
        let (_store, loaded) = Store::open(Some(&dir.0));
        let m = &loaded.patches;
        assert_eq!(m.len(), MAX_UNKNOWN_PATCHES + 1, "the known patch plus the cap: {}", m.len());
        assert!(m.contains_key("metaballs"), "a known patch is never dropped to make room");
    }

    /// The temp file must never be left behind, and the real file must never be
    /// half-written.
    #[test]
    fn a_write_leaves_no_temp_file() {
        let dir = Temp::new("tmp");
        let (store, _) = Store::open(Some(&dir.0));
        for i in 0..20 {
            let mut p = Persisted::default();
            p.players.push(StoredPlayer { device: "abc".into(), seed: i, ..StoredPlayer::default() });
            store.save(p);
        }
        store.flush();
        store.stop();
        let left: Vec<String> =
            std::fs::read_dir(&dir.0).expect("readdir").filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left, vec![FILE.to_string()], "the state directory should hold exactly the state file");
        let text = std::fs::read_to_string(dir.0.join(FILE)).expect("read");
        let back: Persisted = serde_json::from_str(&text).expect("the file parses");
        assert_eq!(back.players[0].seed, 19, "the newest save wins");
    }

    // ------------------------- v3 -> v4 (card 150) -------------------------

    /// A **realistic v3 file**: the shape the deployed service writes today -
    /// a device, the player for it with its tuned parameters, `focus`, and the
    /// memory under its old name. Dummy device names only; nothing here is
    /// copied from a real one.
    const LIVE_V3: &str = r#"{
  "version": 3,
  "devices": [
    { "id": "aa11bb", "name": "the shelf", "instance": "screeny-aa11bb", "address": "", "manual": false }
  ],
  "players": [
    {
      "device": "aa11bb",
      "on": true,
      "piece": "clocks-dials",
      "seed": 4242,
      "params": { "grid": 2.0, "mood": 3.0 },
      "fps": 30.0,
      "settings": {
        "panel": "dithered", "dither": "bayer4",
        "limiter": { "enabled": true, "apl_cap": 0.32, "max_rise_per_s": 1.5 },
        "panel_model": true, "codec_preview": true
      },
      "brightness": 96,
      "paused": false,
      "speed": 0.75
    }
  ],
  "focus": "aa11bb",
  "pieces": {
    "clocks-dials": { "seed": 4242, "params": { "grid": 2.0, "mood": 3.0 } },
    "metaballs": { "seed": 9, "params": { "count": 8.0 } },
    "plasma": { "seed": 3, "params": { "scale": 2.97 } },
    "long-gone": { "seed": 5, "params": { "whatever": 1.0 } }
  }
}"#;

    /// The card's promise: **nothing a v3 file knows is lost**. Every player,
    /// every memory, the panel switch and the brightness arrive intact, only
    /// under the names card 150 gave them.
    #[test]
    fn a_real_v3_file_migrates_to_v4_without_losing_anything() {
        let dir = Temp::new("v3");
        std::fs::write(dir.0.join(FILE), LIVE_V3).expect("write the v3 file");
        let (store, loaded) = Store::open(Some(&dir.0));

        assert_eq!(loaded.version, SCHEMA_VERSION);
        assert_eq!(loaded.devices.len(), 1);
        assert_eq!(loaded.devices[0].id, "aa11bb");
        assert_eq!(loaded.devices[0].name, "the shelf");
        assert_eq!(loaded.devices[0].instance, "screeny-aa11bb");

        assert_eq!(loaded.players.len(), 1);
        let p = &loaded.players[0];
        assert_eq!(p.device, "aa11bb");
        assert!(p.on, "the panel switch");
        assert_eq!(p.patch, "clocks-dials", "`piece` is read as `patch`");
        assert_eq!(p.seed, 4242);
        assert_eq!(p.params, BTreeMap::from([("grid".to_string(), 2.0), ("mood".to_string(), 3.0)]));
        assert_eq!(p.brightness, Some(96), "the brightness policy");
        assert!(!p.paused);
        assert_eq!(p.speed, 0.75);
        // `settings` is read as `output`, in full.
        assert_eq!(p.output.panel, screeny_art::panel::Panel::Dithered);
        assert_eq!(p.output.dither, screeny_art::dither::Dither::Bayer4);
        assert!(p.output.limiter.enabled);
        assert_eq!(p.output.limiter.apl_cap, 0.32);
        assert_eq!(p.output.limiter.max_rise_per_s, 1.5);
        assert!(p.output.panel_model && p.output.codec_preview);
        assert_eq!(loaded.focus, "aa11bb");

        // `pieces` is read as `patches`, entry for entry - including the one
        // for a patch this build has never heard of.
        assert_eq!(loaded.patches.len(), 4);
        assert_eq!(loaded.patches["clocks-dials"].seed, Some(4242));
        assert_eq!(loaded.patches["clocks-dials"].params["mood"], 3.0);
        assert_eq!(loaded.patches["metaballs"].params["count"], 8.0);
        // Two entries for patches this build has not got: `long-gone`, which
        // never existed, and `plasma`, which did until card 178. Both stay.
        assert_eq!(loaded.patches["plasma"].params["scale"], 2.97);
        assert_eq!(loaded.patches["long-gone"].seed, Some(5));

        // The one thing that needed saying: a v3 file names `fps`, which card
        // 161 retired. It names no retired `levels`.
        let said = store.health().repaired;
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].starts_with("`fps`"), "{}", said[0]);
        assert!(!dir.0.join(BAD_FILE).exists(), "a v3 file is migrated, not condemned");
    }

    /// Migrating copies the file aside first, byte for byte, and says where it
    /// put it. An older build can then be put back on the same volume.
    #[test]
    fn migrating_keeps_a_copy_of_the_file_as_it_was() {
        let dir = Temp::new("v3-backup");
        std::fs::write(dir.0.join(FILE), LIVE_V3).expect("write the v3 file");
        let (store, _loaded) = Store::open(Some(&dir.0));

        let kept = dir.0.join(backup_name(3));
        assert_eq!(std::fs::read_to_string(&kept).expect("the backup"), LIVE_V3, "kept byte for byte");
        assert!(dir.0.join(FILE).is_file(), "and the state file itself is still there");

        let why = store.health().recovered.expect("the migration says so");
        assert!(why.contains("v3"), "{why}");
        assert!(why.contains("state.v3.json"), "and where the copy is: {why}");
    }

    /// The next save writes the new names, and only them.
    #[test]
    fn what_is_written_after_the_migration_says_patch_and_output() {
        let dir = Temp::new("v3-written");
        std::fs::write(dir.0.join(FILE), LIVE_V3).expect("write the v3 file");
        let (store, loaded) = Store::open(Some(&dir.0));
        store.save(loaded.clone());
        store.flush();
        store.stop();

        let text = std::fs::read_to_string(dir.0.join(FILE)).expect("read");
        // Card 151: the same claim, one schema on - a v3 file passes through
        // v4's renaming and v5's settings in one start and is written as v5.
        for want in ["\"patches\"", "\"patch\"", "\"output\"", &format!("\"version\": {SCHEMA_VERSION}")] {
            assert!(text.contains(want), "the new file should say {want}:\n{text}");
        }
        // `settings` is card 151's word now - a patch's named settings - but
        // this file has none, so its absence still means what card 150 meant
        // by it: no player writes its output block under the old name.
        for gone in ["\"piece\"", "\"pieces\"", "\"settings\""] {
            assert!(!text.contains(gone), "the new file should not say {gone}:\n{text}");
        }

        // And it reloads as itself, with no second migration.
        let (store2, back) = Store::open(Some(&dir.0));
        assert_eq!(back, loaded);
        assert!(store2.health().recovered.is_none(), "a v4 file is not migrated again");
    }

    /// A file that is already v4 but still spells a player's fields the old
    /// way - a script that has not been updated writing the state directly -
    /// is read, not refused. That is what the `#[serde(alias)]`s are for.
    #[test]
    fn the_old_names_are_still_read_at_v4() {
        let dir = Temp::new("v4-oldnames");
        std::fs::write(
            dir.0.join(FILE),
            format!(
                r#"{{"version":{SCHEMA_VERSION},"focus":"abc",
                     "players":[{{"device":"abc","piece":"metaballs","seed":8,
                                  "settings":{{"dither":"bayer8"}}}}],
                     "pieces":{{"metaballs":{{"seed":8,"params":{{"hue":2.5}}}}}}}}"#
            ),
        )
        .expect("write");
        let (store, loaded) = Store::open(Some(&dir.0));
        assert_eq!(loaded.players[0].patch, "metaballs");
        assert_eq!(loaded.players[0].seed, 8);
        assert_eq!(loaded.players[0].output.dither, screeny_art::dither::Dither::Bayer8);
        assert_eq!(loaded.patches["metaballs"].params["hue"], 2.5);
        assert!(store.health().recovered.is_none(), "same version, so nothing was migrated");
        assert!(!dir.0.join(backup_name(4)).exists(), "and nothing was copied aside");
    }

    /// A v3 file has no `preview` block, so the v1/v2 migration must not run
    /// for it: fed an absent one it would write a memory entry for the default
    /// patch that nobody ever asked for.
    #[test]
    fn migrating_a_v3_file_invents_no_memory() {
        let dir = Temp::new("v3-nopreview");
        std::fs::write(
            dir.0.join(FILE),
            r#"{"version":3,"players":[{"device":"abc","piece":"metaballs","seed":1}],"focus":"abc"}"#,
        )
        .expect("write");
        let (_store, loaded) = Store::open(Some(&dir.0));
        assert!(loaded.patches.is_empty(), "nothing was remembered, so nothing is: {:?}", loaded.patches);
        assert_eq!(loaded.players.len(), 1, "and no second player was invented either");
    }

    // ------------------------- named settings (card 151) -------------------------

    /// The working copy of `metaballs`, tuned however this test wants it.
    fn work(seed: u32, hue: f32, speed: f64) -> Working {
        Working { params: BTreeMap::from([("hue".to_string(), hue)]), seed, speed }
    }

    /// Save, load, and the mark that says it has been moved since.
    #[test]
    fn a_setting_is_saved_loaded_and_says_when_it_has_been_moved() {
        let def = metaballs();
        let mut memory = Memory::new();
        let lava = work(111, 2.5, 0.4);
        remember(&mut memory, def, &lava.params, lava.seed, lava.speed);

        assert_eq!(current_setting(&memory, "metaballs"), DEFAULT_SETTING, "everything starts on Default");
        assert!(modified(&memory, def, &lava), "and a tuned patch is not Default any more");

        assert_eq!(save_setting(&mut memory, def, Some("Lava"), &lava).expect("saved"), "Lava");
        assert_eq!(current_setting(&memory, "metaballs"), "Lava", "saving puts the working copy on what was saved");
        assert!(!modified(&memory, def, &lava), "which is not modified the moment it is written");

        // Move something: the mark comes back, and the setting is untouched.
        let moved = work(111, 3.5, 0.4);
        assert!(modified(&memory, def, &moved));
        let (stored, repaired) = usable_setting(&memory, def, "Lava").expect("still there");
        assert_eq!(stored, lava, "the saved setting did not move with the working copy");
        assert!(repaired.is_empty());

        // A second setting, and loading the first one back.
        let ink = work(222, 1.5, 0.2);
        save_setting(&mut memory, def, Some("Slow ink"), &ink).expect("saved");
        assert_eq!(memory["metaballs"].settings.len(), 2);
        let (back, _) = usable_setting(&memory, def, "Lava").expect("still there");
        assert_eq!(back, lava);

        // Save with no name is "overwrite the one it is on".
        save_setting(&mut memory, def, None, &moved).expect("overwritten");
        assert_eq!(current_setting(&memory, "metaballs"), "Slow ink");
        assert_eq!(usable_setting(&memory, def, "Slow ink").expect("there").0, moved);
    }

    /// Default is the patch's own: synthesised, never stored, and nobody's to
    /// write over, rename or delete.
    #[test]
    fn default_is_the_patchs_own_setting_and_is_read_only() {
        let def = metaballs();
        let mut memory = Memory::new();

        let (default, _) = usable_setting(&memory, def, DEFAULT_SETTING).expect("every patch has one");
        assert!(default.params.is_empty(), "the patch's own defaults, not a copy of them");
        assert_eq!(default.seed, DEFAULT_SEED, "a fixed seed, so Default is one picture");
        assert_eq!(default.speed, 1.0);
        // In any case, and never in the file.
        for spelling in ["default", "DEFAULT", " Default "] {
            assert!(usable_setting(&memory, def, spelling).is_ok(), "{spelling}");
        }
        assert!(memory.is_empty(), "and asking for it wrote nothing down");

        assert!(save_setting(&mut memory, def, None, &default).is_err(), "Save on Default is Save as...");
        assert!(save_setting(&mut memory, def, Some("Default"), &default).is_err());
        assert!(rename_setting(&mut memory, def, None, "Mine").is_err());
        assert!(delete_setting(&mut memory, def, None).is_err());
        assert!(delete_setting(&mut memory, def, Some("default")).is_err(), "in any case");
    }

    /// The name rules, in one place, as a person would meet them.
    #[test]
    fn a_name_is_trimmed_bounded_and_unique_however_it_is_spelled() {
        let def = metaballs();
        let mut memory = Memory::new();
        let w = work(1, 2.5, 1.0);

        assert_eq!(check_name("  Lava  ").expect("trimmed"), "Lava");
        assert_eq!(check_name(&"x".repeat(MAX_NAME_CHARS)).expect("the longest allowed").len(), MAX_NAME_CHARS);
        for bad in ["", "   ", "Default", "DEFAULT", "two\nlines"] {
            assert!(check_name(bad).is_err(), "`{bad}` is not a name a setting may have");
        }
        let too_long = "x".repeat(MAX_NAME_CHARS + 1);
        let why = check_name(&too_long).expect_err("too long");
        assert!(why.contains(&format!("{MAX_NAME_CHARS}")) && why.contains(&format!("{}", MAX_NAME_CHARS + 1)), "{why}");

        // Saving trims, and a name that differs only in case is refused rather
        // than made into a second setting nobody could tell from the first.
        save_setting(&mut memory, def, Some("  Lava  "), &w).expect("saved");
        assert!(memory["metaballs"].settings.contains_key("Lava"));
        let why = save_setting(&mut memory, def, Some("lava"), &w).expect_err("a near-duplicate");
        assert!(why.contains("already a setting called `Lava`"), "{why}");
        // ...but the same name, exactly, is an overwrite, which is the point.
        save_setting(&mut memory, def, Some("Lava"), &work(9, 3.0, 2.0)).expect("overwritten");
        assert_eq!(memory["metaballs"].settings.len(), 1);
        assert_eq!(memory["metaballs"].settings["Lava"].seed, 9);
    }

    /// Bounded: a patch may have [`MAX_SETTINGS`] and the refusal is a sentence
    /// rather than a silently dropped save.
    #[test]
    fn a_patch_may_have_only_so_many_settings() {
        let def = metaballs();
        let mut memory = Memory::new();
        let w = work(1, 2.5, 1.0);
        for i in 0..MAX_SETTINGS {
            save_setting(&mut memory, def, Some(&format!("one {i}")), &w).unwrap_or_else(|e| panic!("{i}: {e}"));
        }
        let why = save_setting(&mut memory, def, Some("one more"), &w).expect_err("that is enough");
        assert!(why.contains(&format!("{MAX_SETTINGS}")), "{why}");
        // Overwriting one of the ones already there is still fine: it is not a
        // new setting.
        save_setting(&mut memory, def, Some("one 0"), &w).expect("an overwrite is not a new one");
        assert_eq!(memory["metaballs"].settings.len(), MAX_SETTINGS);
    }

    /// Rename and delete, including what happens to the name the working copy
    /// is on.
    #[test]
    fn renaming_follows_the_working_copy_and_deleting_leaves_it_playing() {
        let def = metaballs();
        let mut memory = Memory::new();
        let lava = work(111, 2.5, 0.4);
        save_setting(&mut memory, def, Some("Lava"), &lava).expect("saved");
        remember(&mut memory, def, &lava.params, lava.seed, lava.speed);

        assert_eq!(rename_setting(&mut memory, def, None, "Lava lamp").expect("renamed"), "Lava lamp");
        assert_eq!(current_setting(&memory, "metaballs"), "Lava lamp", "the working copy followed its name");
        assert!(!modified(&memory, def, &lava), "and is still not modified");
        assert!(rename_setting(&mut memory, def, Some("Lava"), "Anything").is_err(), "the old name is gone");
        // A re-spelling of its own name is not a clash with itself.
        assert_eq!(rename_setting(&mut memory, def, None, "Lava Lamp").expect("re-spelled"), "Lava Lamp");

        // A second setting. Saving it moves the working copy onto it, so what
        // follows is about deleting one it is *not* on and then one it is.
        save_setting(&mut memory, def, Some("Other"), &work(2, 1.0, 1.0)).expect("saved");
        let why = rename_setting(&mut memory, def, Some("Other"), "lava lamp").expect_err("taken");
        assert!(why.contains("already a setting called `Lava Lamp`"), "{why}");

        let before = memory["metaballs"].params.clone();
        assert_eq!(delete_setting(&mut memory, def, Some("Lava Lamp")).expect("deleted"), "Lava Lamp");
        assert_eq!(current_setting(&memory, "metaballs"), "Other", "deleting another one does not move the name");
        assert_eq!(memory["metaballs"].params, before, "nor what is playing");
        assert!(delete_setting(&mut memory, def, Some("Lava Lamp")).is_err(), "and it is really gone");

        // Deleting the one it *is* on leaves the values playing and the name on
        // Default - which is then honestly "modified".
        assert_eq!(delete_setting(&mut memory, def, None).expect("deleted"), "Other");
        assert_eq!(memory["metaballs"].params, before, "what is playing did not change");
        assert_eq!(current_setting(&memory, "metaballs"), DEFAULT_SETTING);
        assert!(modified(&memory, def, &lava), "values nothing is holding any more");
    }

    /// The case the card is built for: a patch's parameters change between
    /// releases, and a setting saved by the older build still loads.
    ///
    /// One it has **lost** is dropped and said once; one it has **gained**
    /// takes its default by being absent; one out of range is clamped. And the
    /// setting does not read as modified the moment it is loaded, which it
    /// would if `modified` compared against the raw stored values.
    #[test]
    fn a_setting_older_than_the_patch_loads_and_says_what_it_dropped() {
        let def = metaballs();
        let mut memory = Memory::new();
        memory.insert(
            "metaballs".into(),
            PatchMemory {
                settings: BTreeMap::from([(
                    "From before".to_string(),
                    Setting {
                        seed: 7,
                        params: BTreeMap::from([
                            ("hue".to_string(), 2.5),     // still a parameter
                            ("gone".to_string(), 1.0),    // one this build has not got
                            ("speed".to_string(), 999.0), // out of this build's range
                        ]),
                        speed: 0.5,
                    },
                )]),
                ..PatchMemory::default()
            },
        );

        let (usable, repaired) = usable_setting(&memory, def, "From before").expect("it still loads");
        assert_eq!(usable.params["hue"], 2.5, "the good value survived");
        assert!(!usable.params.contains_key("gone"), "a parameter the patch has lost is dropped");
        assert_eq!(usable.params["speed"], spec(def, "speed").max, "out of range is clamped");
        assert!(!usable.params.contains_key("size"), "one the patch has gained is simply its default");
        assert_eq!(usable.seed, 7);
        assert_eq!(usable.speed, 0.5);
        assert_eq!(repaired.len(), 2, "one sentence each, and never an error: {repaired:?}");
        assert!(repaired.iter().all(|r| r.contains("From before")), "each says which setting: {repaired:?}");

        // Load it, and it is not modified - although the file still holds the
        // values this build cannot use.
        memory.get_mut("metaballs").expect("there").setting = "From before".into();
        remember(&mut memory, def, &usable.params, usable.seed, usable.speed);
        assert!(!modified(&memory, def, &usable), "a repaired setting must not read as modified for ever");
        assert!(memory["metaballs"].settings["From before"].params.contains_key("gone"), "and the file is left as it was");
    }

    /// A setting a patch has not got, and a name that has gone from under the
    /// working copy's feet.
    #[test]
    fn asking_for_a_setting_that_is_not_there_says_so() {
        let def = metaballs();
        let mut memory = Memory::new();
        let why = usable_setting(&memory, def, "Nope").expect_err("no such setting");
        assert!(why.contains("no setting called `Nope`"), "{why}");
        // A working copy pointed at a name nothing answers to is modified: there
        // is nothing left for it to be equal to.
        memory.insert("metaballs".into(), PatchMemory { setting: "Ghost".into(), ..PatchMemory::default() });
        assert!(modified(&memory, def, &work(1, 2.5, 1.0)));
    }

    /// v4 -> v5: a realistic v4 file - the shape the live service writes today,
    /// with dummy device names - loads, keeps everything, and comes back as v5.
    ///
    /// The one thing that **moves** is speed: a player at 0.4x hands that to
    /// the patch it is on, so the working copy is where the panel actually was.
    #[test]
    fn a_real_v4_file_migrates_to_v5() {
        let dir = Temp::new("v4");
        let v4 = r#"{
  "version": 4,
  "devices": [
    { "id": "aa11bb", "name": "the shelf", "instance": "screeny-aa11bb", "address": "", "manual": false }
  ],
  "players": [
    {
      "device": "aa11bb",
      "on": true,
      "patch": "clocks-dials",
      "seed": 4242,
      "params": { "mood": 3.0 },
      "fps": 30.0,
      "output": {
        "dither": "bayer4",
        "limiter": { "enabled": true, "apl_cap": 0.4, "max_rise_per_s": 2.0 },
        "panel_model": true, "codec_preview": true
      },
      "brightness": 96,
      "paused": false,
      "speed": 0.4
    }
  ],
  "focus": "aa11bb",
  "patches": {
    "clocks-numerals": { "seed": 0 },
    "metaballs": { "seed": 222, "params": { "count": 8.0 } },
    "clocks-dials": { "seed": 4242, "params": { "mood": 3.0 } }
  }
}"#;
        std::fs::write(dir.0.join(FILE), v4).expect("write the v4 file");
        let (store, loaded) = Store::open(Some(&dir.0));

        assert_eq!(loaded.version, SCHEMA_VERSION);
        assert_eq!(loaded.devices.len(), 1);
        assert_eq!(loaded.players.len(), 1);
        assert_eq!(loaded.players[0].patch, "clocks-dials");
        assert_eq!(loaded.players[0].speed, 0.4);
        assert_eq!(loaded.focus, "aa11bb");

        // The memory is v4's, entry for entry, and nothing was invented.
        assert_eq!(loaded.patches.len(), 3);
        assert_eq!(loaded.patches["metaballs"].params["count"], 8.0);
        assert_eq!(loaded.patches["metaballs"].seed, Some(222));
        assert!(loaded.patches.values().all(|e| e.settings.is_empty()), "a v4 file has no settings to carry");
        assert!(loaded.patches.values().all(|e| e.setting.is_empty()), "so every patch is on Default");
        // The one thing that moves, and only for the patch that was playing.
        assert_eq!(loaded.patches["clocks-dials"].speed, Some(0.4), "the panel was at 0.4x and comes back at 0.4x");
        assert_eq!(loaded.patches["metaballs"].speed, None, "a patch nothing was playing is left alone");

        assert!(store.health().recovered.is_some_and(|w| w.contains("v4")), "it says so once");
        let said = store.health().repaired;
        assert_eq!(said.len(), 1, "a good v4 file needs only the retired `fps` said: {said:?}");
        assert!(said[0].starts_with("`fps`"), "{}", said[0]);
        assert_eq!(std::fs::read_to_string(dir.0.join(backup_name(4))).expect("the backup"), v4, "kept byte for byte");
        assert!(!dir.0.join(BAD_FILE).exists(), "a v4 file is migrated, not condemned");
    }

    /// And the migration is **run once**: the guard means a file that has
    /// already been through a step does not run it again. A v5 file whose
    /// player is at 1.0x but whose patch remembers 0.4x keeps the 0.4x, which
    /// is exactly what a second run of v4 -> v5 would overwrite.
    #[test]
    fn a_v5_file_does_not_run_the_migration_again() {
        let dir = Temp::new("v5-once");
        let v5 = format!(
            r#"{{"version":{SCHEMA_VERSION},"focus":"abc",
                 "players":[{{"device":"abc","patch":"metaballs","seed":8,"speed":1.0}}],
                 "patches":{{"metaballs":{{"seed":8,"speed":0.4,"setting":"Lava",
                              "settings":{{"Lava":{{"seed":8,"params":{{"hue":2.5}},"speed":0.4}}}}}}}}}}"#
        );
        std::fs::write(dir.0.join(FILE), &v5).expect("write");
        let (store, loaded) = Store::open(Some(&dir.0));
        assert!(store.health().recovered.is_none(), "same version, so nothing was migrated");
        assert!(!dir.0.join(backup_name(5)).exists(), "and nothing was copied aside");
        assert_eq!(loaded.patches["metaballs"].speed, Some(0.4), "the working copy's own speed, not the player's");
        assert_eq!(loaded.patches["metaballs"].setting, "Lava");
        assert_eq!(loaded.patches["metaballs"].settings["Lava"].params["hue"], 2.5);
        assert_eq!(loaded.patches["metaballs"].settings["Lava"].speed, 0.4);
    }

    /// A v1 file goes all the way in one start: v1 -> v3 -> v5.
    #[test]
    fn a_v1_file_comes_all_the_way_up_in_one_start() {
        let dir = Temp::new("v1-to-v5");
        std::fs::write(
            dir.0.join(FILE),
            r#"{
              "version": 1,
              "players": [ { "device": "abc", "piece": "clocks-dials", "seed": 2, "params": { "mood": 3.0 }, "speed": 0.5 } ],
              "preview":   { "piece": "metaballs", "seed": 1, "params": { "count": 8.0 }, "speed": 2.0 }
            }"#,
        )
        .expect("write");
        let (store, loaded) = Store::open(Some(&dir.0));
        assert_eq!(loaded.version, SCHEMA_VERSION);
        assert_eq!(loaded.patches["clocks-dials"].params["mood"], 3.0, "v1's merge still happened");
        assert_eq!(loaded.patches["clocks-dials"].speed, Some(0.5), "and each context's speed came with it");
        assert_eq!(loaded.patches["metaballs"].speed, Some(2.0));
        assert!(loaded.patches.values().all(|e| e.settings.is_empty()));
        let why = store.health().recovered.expect("it says so");
        assert!(why.contains("v1") && why.contains(&format!("v{SCHEMA_VERSION}")), "{why}");
    }

    /// The file can be hand-edited, so every way a `settings` block can be
    /// wrong costs that block and nothing else - never the file.
    #[test]
    fn a_hand_edited_settings_block_costs_only_what_is_wrong() {
        let dir = Temp::new("v5-garbage");
        let file = format!(
            r#"{{"version":{SCHEMA_VERSION},
                 "patches":{{"metaballs":{{"seed":1,"speed":"fast","setting":"Ghost","settings":{{
                   "Good":     {{"seed":5,"params":{{"hue":2.5}},"speed":0.5}},
                   "Bad seed": {{"seed":"eleven","params":{{"hue":2.0}}}},
                   "Bad speed":{{"seed":5,"speed":-3}},
                   "Bad param":{{"seed":5,"params":{{"hue":"wide"}}}},
                   "Not an object": 7,
                   "":         {{"seed":5}},
                   "Default":  {{"seed":5}},
                   "good":     {{"seed":6}}
                 }}}}}}}}"#
        );
        std::fs::write(dir.0.join(FILE), &file).expect("write");
        let (store, loaded) = Store::open(Some(&dir.0));
        let entry = &loaded.patches["metaballs"];

        assert!(!dir.0.join(BAD_FILE).exists(), "one bad value never condemns the file");
        assert_eq!(entry.seed, Some(1), "and never costs the rest of the entry");
        assert_eq!(entry.speed, None, "a speed that is not a speed is forgotten");
        assert!(entry.setting.is_empty(), "a name nothing answers to is not a name it is on");

        assert_eq!(entry.settings["Good"].params["hue"], 2.5);
        assert_eq!(entry.settings["Good"].speed, 0.5);
        assert_eq!(entry.settings["Bad seed"].seed, DEFAULT_SEED, "a seed that is not a seed takes Default's");
        assert_eq!(entry.settings["Bad speed"].speed, 1.0, "a speed that is not a speed is 1.00x");
        assert!(entry.settings["Bad param"].params.is_empty(), "a value that is not a number is its default");
        for gone in ["Not an object", "", "Default", "good"] {
            assert!(!entry.settings.contains_key(gone), "`{gone}` is not a setting this build will keep");
        }
        assert_eq!(entry.settings.len(), 4);

        let said = store.health().repaired;
        assert!(said.len() >= 7, "one sentence per thing corrected: {said:?}");
        assert!(said.iter().any(|s| s.contains("Ghost")), "including the name it thought it was on: {said:?}");
    }

    /// A file with more settings than a patch may have is cut to the bound, not
    /// refused: a hand-edited file cannot make the studio grow without limit.
    #[test]
    fn a_file_with_too_many_settings_is_cut_to_the_bound() {
        let dir = Temp::new("v5-too-many");
        let settings: Vec<String> = (0..MAX_SETTINGS + 5).map(|i| format!(r#""one {i:03}":{{"seed":{i}}}"#)).collect();
        let file = format!(
            r#"{{"version":{SCHEMA_VERSION},"patches":{{"metaballs":{{"seed":1,"settings":{{{}}}}}}}}}"#,
            settings.join(",")
        );
        std::fs::write(dir.0.join(FILE), &file).expect("write");
        let (store, loaded) = Store::open(Some(&dir.0));
        assert_eq!(loaded.patches["metaballs"].settings.len(), MAX_SETTINGS);
        assert!(store.health().repaired.iter().any(|s| s.contains(&format!("{MAX_SETTINGS}"))), "{:?}", store.health().repaired);
    }
}
