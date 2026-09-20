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

use screeny_art::piece::{ParamSpec, PieceDef};
use screeny_art::Settings;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// The schema this build writes and is willing to read.
///
/// v2 (card 165) adds the per-piece settings memory to the preview and to
/// every player. v1 files are migrated: what a context was playing at the time
/// becomes that piece's first memory, so nobody loses what they have today.
pub const SCHEMA_VERSION: u32 = 2;
/// The file, inside the state directory.
pub const FILE: &str = "state.json";
/// Where the last unreadable state file is kept. One fixed name: a server that
/// runs for months must not accumulate rubble.
pub const BAD_FILE: &str = "state.bad.json";

/// How many memories for pieces this build has never heard of are kept.
///
/// The studio itself only ever writes a memory for a piece it can play, so it
/// is already bounded by the number of pieces in the binary. This is the bound
/// on the other direction: a file edited by hand, or written by a build with
/// pieces this one does not have, cannot grow without limit.
pub const MAX_UNKNOWN_PIECES: usize = 64;

/// How many "this is what I had to correct" sentences are kept for the status
/// route. The log has them all; the dashboard does not need a novel.
const MAX_REPAIRS: usize = 16;

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
    /// reachable - that is the normal case after a power cut.
    pub players: Vec<StoredPlayer>,
    /// The design view's own player, which is not tied to a device.
    pub preview: StoredPreview,
}

impl Default for Persisted {
    fn default() -> Self {
        Persisted {
            version: SCHEMA_VERSION,
            devices: Vec::new(),
            players: Vec::new(),
            preview: StoredPreview::default(),
        }
    }
}

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

/// What one panel plays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredPlayer {
    /// The device id this plays to.
    pub device: String,
    /// False leaves the player configured but silent - the panel is released.
    pub on: bool,
    pub piece: String,
    pub seed: u32,
    pub params: BTreeMap<String, f32>,
    pub fps: f64,
    pub settings: Settings,
    /// Brightness policy: a fixed level to apply whenever the link comes up,
    /// or `None` to leave whatever the device has. Never raised above the cap
    /// the device reports back.
    pub brightness: Option<u8>,
    /// This panel's own per-piece settings memory (card 165). Schema v2.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub memory: Memory,
}

impl Default for StoredPlayer {
    fn default() -> Self {
        StoredPlayer {
            device: String::new(),
            on: true,
            piece: default_piece().to_string(),
            seed: 1,
            params: BTreeMap::new(),
            fps: 60.0,
            settings: Settings::default(),
            brightness: None,
            memory: Memory::new(),
        }
    }
}

/// The design view: what card 105 kept in `StudioState`, plus the two things
/// it explicitly left for this card - whether the preview is being sent to a
/// panel, and to which one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredPreview {
    pub piece: String,
    pub seed: u32,
    pub params: BTreeMap<String, f32>,
    pub settings: Settings,
    pub paused: bool,
    pub speed: f64,
    pub fps: f64,
    /// "Send to panel" - which, before this card, lived only in the browser's
    /// `localStorage` and in the half-second heartbeat.
    pub panel_on: bool,
    /// A device id, a name or an address. Empty means "the first panel found".
    pub panel_to: String,
    /// The design view's own per-piece settings memory (card 165). Schema v2.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub memory: Memory,
}

impl Default for StoredPreview {
    fn default() -> Self {
        StoredPreview {
            piece: default_piece().to_string(),
            seed: 0,
            params: BTreeMap::new(),
            settings: Settings::default(),
            paused: false,
            speed: 1.0,
            fps: 60.0,
            panel_on: false,
            panel_to: String::new(),
            memory: Memory::new(),
        }
    }
}

// ----------------------------------------------- the per-piece memory (165) ---

/// What one context remembers about one piece.
///
/// Only what *differs* from the piece's defaults is kept, so a piece whose
/// defaults improve in a later release improves for everybody who never
/// touched that parameter - and the file stays small.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PieceMemory {
    /// The seed this piece was last left on. `None` means "never chosen", in
    /// which case switching to it keeps whatever seed the context is on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u32>,
    /// Parameter values that differ from the defaults, by param id.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, f32>,
}

impl PieceMemory {
    /// Nothing worth writing down.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seed.is_none() && self.params.is_empty()
    }
}

/// One context's memory: piece id -> what it was left set to.
///
/// There is one of these for the design view and one for each device player,
/// because "what panel X plays" and "what I am fiddling with" are different
/// things (card 106 made that split explicit).
pub type Memory = BTreeMap<String, PieceMemory>;

/// Write down what `def` is set to now.
///
/// A value equal to the piece's default is *removed* rather than stored: that
/// is the whole reason a later release's better default still reaches the
/// people who never touched that slider.
pub fn remember(memory: &mut Memory, def: &PieceDef, params: &BTreeMap<String, f32>, seed: u32) {
    let entry = memory.entry(def.id.to_string()).or_default();
    entry.seed = Some(seed);
    entry.params.clear();
    for spec in def.params {
        if let Some(v) = params.get(spec.id) {
            let v = spec.sanitise(*v);
            // `!=` on f32 is exactly right here: the question is whether this
            // is still literally the default, not whether it is close to it.
            if v != spec.default {
                entry.params.insert(spec.id.to_string(), v);
            }
        }
    }
}

/// Forget the parameters remembered for `piece`, keeping its seed.
///
/// This is what "Reset" means: back to the defaults, and *stay* there, rather
/// than being handed the old values again on the next switch back.
pub fn forget_params(memory: &mut Memory, piece: &str) {
    if let Some(entry) = memory.get_mut(piece) {
        entry.params.clear();
        if entry.is_empty() {
            memory.remove(piece);
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
/// | a parameter this build's piece does not have | ignored |
/// | a value outside the spec's range | clamped to the range |
/// | a value that is not a finite number | the piece's default |
///
/// Corrections are **written back** into the memory, so a file that needed
/// fixing is fixed once rather than complained about on every switch - which
/// is also what makes "logged once" true without a set of things already said.
/// `who` names the context for that one log line.
#[must_use]
pub fn recall(memory: &mut Memory, def: &PieceDef, who: &str) -> (BTreeMap<String, f32>, Option<u32>) {
    let Some(entry) = memory.get_mut(def.id) else {
        return (BTreeMap::new(), None);
    };
    let (usable, repaired) = usable_params(&entry.params, def.params);
    if !repaired.is_empty() {
        eprintln!("studio: {who}: the remembered settings for `{}`: {}", def.id, repaired.join("; "));
        entry.params = usable.clone();
        if entry.is_empty() {
            memory.remove(def.id);
        }
    }
    (usable, entry_seed(memory, def.id))
}

fn entry_seed(memory: &Memory, piece: &str) -> Option<u32> {
    memory.get(piece).and_then(|e| e.seed)
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

/// Read a `memory` object out of a file as forgivingly as the card asks.
///
/// This runs on `serde_json::Value` rather than through serde's `f32`
/// deliberately: `BTreeMap<String, f32>` refuses a `null`, a string or an
/// object, and a refusal here would condemn the **whole file** to
/// `state.bad.json` under card 106's rules. One bad value must cost exactly
/// that one value.
fn clean_memory(raw: Option<&serde_json::Value>, repaired: &mut Vec<String>) -> Memory {
    let Some(serde_json::Value::Object(entries)) = raw else {
        if raw.is_some_and(|v| !v.is_null()) {
            repaired.push("the remembered settings were not an object; forgotten".into());
        }
        return Memory::new();
    };
    let mut memory = Memory::new();
    let mut unknown = 0usize;
    for (piece, value) in entries {
        // An unknown piece id keeps its entry - a piece that comes back in a
        // later release gets its settings back - but only so many of them.
        if screeny_art::piece::find(piece).is_none() {
            unknown += 1;
            if unknown > MAX_UNKNOWN_PIECES {
                repaired.push(format!("`{piece}` is not a piece here and there were already {MAX_UNKNOWN_PIECES} such entries; dropped"));
                continue;
            }
        }
        let serde_json::Value::Object(entry) = value else {
            repaired.push(format!("what was remembered for `{piece}` was not an object; forgotten"));
            continue;
        };
        let seed = match entry.get("seed") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => match v.as_u64().and_then(|n| u32::try_from(n).ok()) {
                Some(n) => Some(n),
                None => {
                    repaired.push(format!("the seed remembered for `{piece}` was not a seed; forgotten"));
                    None
                }
            },
        };
        let mut params = BTreeMap::new();
        if let Some(serde_json::Value::Object(ps)) = entry.get("params") {
            for (id, v) in ps {
                match v.as_f64().map(|f| f as f32).filter(|f| f.is_finite()) {
                    Some(f) => {
                        params.insert(id.clone(), f);
                    }
                    None => repaired.push(format!("`{piece}`'s remembered `{id}` was not a number; back to its default")),
                }
            }
        } else if entry.get("params").is_some_and(|v| !v.is_null()) {
            repaired.push(format!("the parameters remembered for `{piece}` were not an object; forgotten"));
        }
        let m = PieceMemory { seed, params };
        if !m.is_empty() {
            memory.insert(piece.clone(), m);
        }
    }
    memory
}

/// A file that did not name a schema version at all.
fn no_version() -> u32 {
    0
}

/// The piece a fresh studio starts on.
#[must_use]
pub fn default_piece() -> &'static str {
    screeny_art::pieces::ALL[0].id
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
    /// Remembered settings this build could not use as written and silently
    /// corrected (card 165). Not a fault, never a 503: a value being out of
    /// range after a piece was re-ranged is exactly what the memory is meant
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
    // would reject - and the per-piece memory has to be lifted out and cleaned
    // before serde sees it, because a single bad value in there must cost that
    // value and not the whole file.
    let mut raw: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return keep_the_file(format!("the state file could not be parsed ({e})")),
    };

    if let Some(v) = raw.get("version").and_then(serde_json::Value::as_u64) {
        if v > u64::from(SCHEMA_VERSION) {
            let keep = path.with_file_name(format!("state.v{v}.json"));
            let moved = std::fs::rename(path, &keep).is_ok();
            let where_ = if moved { format!("; kept as {}", keep.display()) } else { String::new() };
            return Loaded::fresh(Some(format!(
                "the state file is schema v{v} and this build understands v{SCHEMA_VERSION}{where_}"
            )));
        }
    }

    let mut repaired = Vec::new();
    let memories = lift_memories(&mut raw, &mut repaired);

    let mut state: Persisted = match serde_json::from_value(raw) {
        Ok(p) => p,
        Err(e) => return keep_the_file(format!("the state file could not be parsed ({e})")),
    };
    let was = state.version;
    state.version = SCHEMA_VERSION;
    put_memories_back(&mut state, memories);
    if was < 2 {
        migrate_v1_to_v2(&mut state);
    }
    repaired.truncate(MAX_REPAIRS);
    let recovered =
        (was != SCHEMA_VERSION).then(|| format!("the state file was schema v{was}; migrated to v{SCHEMA_VERSION}"));
    Loaded { state, recovered, repaired }
}

/// Take every `memory` object out of the raw JSON and clean it.
///
/// `None` for the preview, then one per player in the order they appear, which
/// is the order [`put_memories_back`] hands them out again.
fn lift_memories(raw: &mut serde_json::Value, repaired: &mut Vec<String>) -> (Memory, Vec<Memory>) {
    let preview = raw.get_mut("preview").map(|p| {
        let m = clean_memory(p.get("memory"), repaired);
        if let Some(o) = p.as_object_mut() {
            o.remove("memory");
        }
        m
    });
    let players = raw.get_mut("players").and_then(serde_json::Value::as_array_mut).map(|list| {
        list.iter_mut()
            .map(|p| {
                let m = clean_memory(p.get("memory"), repaired);
                if let Some(o) = p.as_object_mut() {
                    o.remove("memory");
                }
                m
            })
            .collect()
    });
    (preview.unwrap_or_default(), players.unwrap_or_default())
}

fn put_memories_back(state: &mut Persisted, (preview, players): (Memory, Vec<Memory>)) {
    state.preview.memory = preview;
    for (player, memory) in state.players.iter_mut().zip(players) {
        player.memory = memory;
    }
}

/// v1 -> v2: what each context was playing becomes that piece's first memory.
///
/// v1 kept one `params` map per context, for the *current* piece only. That is
/// exactly one entry's worth of memory, so nobody loses the tuning they have
/// today - and because [`remember`] drops anything equal to the piece's
/// default, a v1 file's full parameter dump comes out of the migration as just
/// the values that were actually moved.
fn migrate_v1_to_v2(state: &mut Persisted) {
    for player in &mut state.players {
        if let Some(def) = screeny_art::piece::find(&player.piece) {
            remember(&mut player.memory, def, &player.params, player.seed);
        }
    }
    let preview = &mut state.preview;
    if let Some(def) = screeny_art::piece::find(&preview.piece) {
        remember(&mut preview.memory, def, &preview.params, preview.seed);
    }
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
        want.players.push(StoredPlayer { device: "abc123".into(), piece: "plasma".into(), seed: 7, ..StoredPlayer::default() });
        want.preview.panel_on = true;
        want.preview.panel_to = "screeny-4a00a4".into();
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
        std::fs::write(dir.0.join(FILE), r#"{"players":[{"device":"abc","piece":"plasma","seed":3}]}"#).expect("write");
        let (store, loaded) = Store::open(Some(&dir.0));
        assert_eq!(loaded.version, SCHEMA_VERSION);
        assert_eq!(loaded.players.len(), 1);
        assert_eq!(loaded.players[0].seed, 3);
        // Fields the old file did not have take their defaults, not zeroes.
        assert!(loaded.players[0].on);
        assert_eq!(loaded.players[0].fps, 60.0);
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

    // ------------------------- the per-piece memory (card 165) -------------------------

    fn plasma() -> &'static PieceDef {
        screeny_art::piece::find("plasma").expect("plasma is in every build")
    }

    fn spec(def: &PieceDef, id: &str) -> ParamSpec {
        *def.params.iter().find(|s| s.id == id).expect("a parameter of this piece")
    }

    /// The decision the card is built on: only what a human actually moved is
    /// written down, so a later release's better default reaches everybody who
    /// never touched that slider.
    #[test]
    fn only_what_differs_from_the_defaults_is_remembered() {
        let def = plasma();
        let mut memory = Memory::new();
        let mut live: BTreeMap<String, f32> = def.params.iter().map(|s| (s.id.to_string(), s.default)).collect();
        remember(&mut memory, def, &live, 7);
        assert_eq!(memory["plasma"].params, BTreeMap::new(), "untouched defaults are not worth a byte");
        assert_eq!(memory["plasma"].seed, Some(7));

        live.insert("scale".into(), 2.5);
        remember(&mut memory, def, &live, 7);
        assert_eq!(memory["plasma"].params.len(), 1);
        assert_eq!(memory["plasma"].params["scale"], 2.5);

        // And putting it back where it started forgets it again.
        live.insert("scale".into(), spec(def, "scale").default);
        remember(&mut memory, def, &live, 7);
        assert!(memory["plasma"].params.is_empty());
    }

    /// Reset means "back to the defaults and *stay* there", so the parameters
    /// are forgotten. The seed is not: the seed is not what Reset is about, and
    /// throwing it away would make a switch back rebuild the piece on whatever
    /// seed the *other* piece happened to be on.
    #[test]
    fn reset_forgets_the_parameters_and_keeps_the_seed() {
        let def = plasma();
        let mut memory = Memory::new();
        remember(&mut memory, def, &BTreeMap::from([("scale".to_string(), 2.5)]), 99);
        forget_params(&mut memory, "plasma");
        assert_eq!(memory["plasma"].seed, Some(99));
        assert!(memory["plasma"].params.is_empty());

        // An entry with nothing left in it at all goes away entirely.
        memory.insert("ghost".into(), PieceMemory { seed: None, params: BTreeMap::from([("x".into(), 1.0)]) });
        forget_params(&mut memory, "ghost");
        assert!(!memory.contains_key("ghost"));
    }

    /// Every error case the card lists, one value at a time, and the rule that
    /// matters: one bad value never costs the others.
    #[test]
    fn a_value_this_build_cannot_use_becomes_the_default_and_the_rest_survive() {
        let def = plasma();
        let scale = spec(def, "scale");
        let remembered = BTreeMap::from([
            ("scale".to_string(), 2.5),            // good, and must survive all of this
            ("gone".to_string(), 1.0),             // a parameter this build does not have
            ("drift".to_string(), 99.0),           // above the range
            ("cycle".to_string(), -99.0),          // below the range
            ("bands".to_string(), f32::NAN),       // not a number
            ("black".to_string(), f32::INFINITY),  // not a number either
            ("hue".to_string(), spec(def, "hue").default), // already the default
        ]);
        let (usable, repaired) = usable_params(&remembered, def.params);

        assert_eq!(usable["scale"], 2.5, "the good value survived every bad one");
        assert!(!usable.contains_key("gone"), "an unknown parameter is ignored");
        assert_eq!(usable["drift"], spec(def, "drift").max, "out of range is clamped, as the slider would");
        assert_eq!(usable["cycle"], spec(def, "cycle").min);
        assert!(!usable.contains_key("bands"), "a NaN falls back to the default");
        assert!(!usable.contains_key("black"), "an infinity falls back to the default");
        assert!(!usable.contains_key("hue"), "a value that is already the default is not stored");
        assert_eq!(repaired.len(), 5, "one sentence per correction: {repaired:?}");
        assert_eq!(scale.sanitise(2.5), 2.5);
    }

    /// The correction is written back, so it happens once rather than on every
    /// switch - which is what makes "logged once" true without keeping a set of
    /// things already said.
    #[test]
    fn a_correction_is_made_once_and_written_back() {
        let def = plasma();
        let mut memory = Memory::new();
        memory.insert(
            "plasma".into(),
            PieceMemory { seed: Some(5), params: BTreeMap::from([("scale".into(), 99.0), ("gone".into(), 1.0)]) },
        );
        let (first, seed) = recall(&mut memory, def, "a test");
        assert_eq!(seed, Some(5));
        assert_eq!(first["scale"], spec(def, "scale").max);
        assert_eq!(memory["plasma"].params, first, "the file's copy was corrected too");
        // Second time round there is nothing left to correct, so nothing to say.
        let (again, _) = recall(&mut memory, def, "a test");
        assert_eq!(again, first);
    }

    /// The whole point: switch away, switch back, and it is as you left it.
    #[test]
    fn a_memory_round_trips_through_the_file() {
        let dir = Temp::new("memory-roundtrip");
        let (store, _) = Store::open(Some(&dir.0));
        let mut want = Persisted::default();
        want.preview.memory.insert("plasma".into(), PieceMemory { seed: Some(3), params: BTreeMap::from([("scale".into(), 2.5)]) });
        want.players.push(StoredPlayer {
            device: "abc".into(),
            memory: Memory::from([("metaballs".to_string(), PieceMemory { seed: Some(9), params: BTreeMap::from([("count".into(), 8.0)]) })]),
            ..StoredPlayer::default()
        });
        store.save(want.clone());
        store.flush();
        store.stop();

        let text = std::fs::read_to_string(dir.0.join(FILE)).expect("read");
        assert!(text.contains("\"memory\""), "the memory is in the state file, not somewhere else:\n{text}");
        let (_s2, back) = Store::open(Some(&dir.0));
        assert_eq!(back, want);
        assert_eq!(back.version, SCHEMA_VERSION);
    }

    /// A v1 file as the deployed service has one today (the shape is card 106's
    /// Log). What each context was playing becomes that piece's first memory:
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
      "piece": "plasma",
      "seed": 4242,
      "params": {
        "scale": 2.5, "drift": 0.35, "cycle": 0.12, "bands": 1.5, "colours": 32.0,
        "black": 0.45, "hue": 300.0, "spread": 140.0, "dither": 1.0
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
    "piece": "metaballs",
    "seed": 7,
    "params": { "count": 7.0, "speed": 0.5, "size": 1.0, "hue": 20.0, "spread": 200.0, "samples": 4.0 },
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
        assert_eq!(loaded.players[0].piece, "plasma");
        assert_eq!(loaded.players[0].seed, 4242);
        assert_eq!(loaded.players[0].brightness, Some(96));
        assert_eq!(loaded.preview.piece, "metaballs");
        assert_eq!(loaded.preview.panel_to, "screeny-4a00a4");
        assert!(loaded.preview.panel_on);

        // And what each context was playing is now its first memory - only the
        // values that were actually moved, not the whole v1 parameter dump.
        let player = &loaded.players[0].memory;
        assert_eq!(player["plasma"].seed, Some(4242));
        assert_eq!(player["plasma"].params, BTreeMap::from([("scale".to_string(), 2.5)]), "only `scale` was off its default");
        let preview = &loaded.preview.memory;
        assert_eq!(preview["metaballs"].seed, Some(7));
        assert_eq!(preview["metaballs"].params, BTreeMap::from([("count".to_string(), 7.0)]));

        assert!(store.health().recovered.is_some_and(|w| w.contains("v1")), "the migration says so once");
        assert!(store.health().repaired.is_empty(), "a good v1 file needs no repairs");
        assert!(!dir.0.join(BAD_FILE).exists(), "a v1 file is migrated, not condemned");
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
  "preview": {{
    "piece": "plasma",
    "memory": {{
      "plasma":   {{ "seed": 11, "params": {{ "scale": 2.5, "drift": null, "cycle": "fast", "bands": {{}}, "colours": [] }} }},
      "metaballs": {{ "seed": "not a seed", "params": {{ "count": 8.0 }} }},
      "no-such-piece": {{ "seed": 4, "params": {{ "whatever": 1.0 }} }},
      "testcard": "not an object at all",
      "clocks-dials": {{ "params": "not an object either" }}
    }}
  }}
}}"#
        );
        std::fs::write(dir.0.join(FILE), &file).expect("write");
        let (store, loaded) = Store::open(Some(&dir.0));

        assert!(!dir.0.join(BAD_FILE).exists(), "a bad value must never condemn the file");
        let m = &loaded.preview.memory;
        assert_eq!(m["plasma"].seed, Some(11));
        assert_eq!(m["plasma"].params, BTreeMap::from([("scale".to_string(), 2.5)]), "one good value, four bad ones dropped");
        assert_eq!(m["metaballs"].seed, None, "a seed that is not a seed is forgotten");
        assert_eq!(m["metaballs"].params["count"], 8.0, "and the rest of that piece's memory survives it");
        assert!(m.contains_key("no-such-piece"), "an unknown piece keeps its entry, so a piece that comes back gets it");
        assert!(!m.contains_key("testcard"), "an entry that is not an object is forgotten");
        assert!(!m.contains_key("clocks-dials"), "so is one with nothing usable left in it");
        assert!(!store.health().repaired.is_empty(), "and the server says what it had to correct");
        assert!(store.health().last_error.is_none());
    }

    /// The bound. The studio only ever writes a memory for a piece it can play,
    /// so this is the other direction: a hand-edited file cannot make the state
    /// file grow for ever.
    #[test]
    fn unknown_pieces_are_capped() {
        let dir = Temp::new("cap");
        let entries: Vec<String> =
            (0..MAX_UNKNOWN_PIECES + 20).map(|i| format!(r#""ghost-{i:03}": {{"seed": {i}}}"#)).collect();
        let file = format!(
            r#"{{"version":{SCHEMA_VERSION},"preview":{{"piece":"plasma","memory":{{{},"plasma":{{"seed":1}}}}}}}}"#,
            entries.join(",")
        );
        std::fs::write(dir.0.join(FILE), &file).expect("write");
        let (_store, loaded) = Store::open(Some(&dir.0));
        let m = &loaded.preview.memory;
        assert_eq!(m.len(), MAX_UNKNOWN_PIECES + 1, "the known piece plus the cap: {}", m.len());
        assert!(m.contains_key("plasma"), "a known piece is never dropped to make room");
    }

    /// The temp file must never be left behind, and the real file must never be
    /// half-written.
    #[test]
    fn a_write_leaves_no_temp_file() {
        let dir = Temp::new("tmp");
        let (store, _) = Store::open(Some(&dir.0));
        for i in 0..20 {
            let mut p = Persisted::default();
            p.preview.seed = i;
            store.save(p);
        }
        store.flush();
        store.stop();
        let left: Vec<String> =
            std::fs::read_dir(&dir.0).expect("readdir").filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left, vec![FILE.to_string()], "the state directory should hold exactly the state file");
        let text = std::fs::read_to_string(dir.0.join(FILE)).expect("read");
        let back: Persisted = serde_json::from_str(&text).expect("the file parses");
        assert_eq!(back.preview.seed, 19, "the newest save wins");
    }
}
