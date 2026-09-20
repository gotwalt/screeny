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

use screeny_art::Settings;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// The schema this build writes and is willing to read.
pub const SCHEMA_VERSION: u32 = 1;
/// The file, inside the state directory.
pub const FILE: &str = "state.json";
/// Where the last unreadable state file is kept. One fixed name: a server that
/// runs for months must not accumulate rubble.
pub const BAD_FILE: &str = "state.bad.json";

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
        }
    }
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
}

// ---------------------------------------------------------------- loading ---

/// Read the state file, whatever is in it.
///
/// Returns the state to start from and, when the file could not be used, one
/// sentence saying why - already logged, and worth putting on the dashboard.
/// This function has no failure mode: that is the point of it.
fn load(path: &Path) -> (Persisted, Option<String>) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        // No file is the normal first run, not a problem to report.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Persisted::default(), None),
        Err(e) => {
            return (Persisted::default(), Some(format!("{} could not be read ({e}); starting from defaults", path.display())));
        }
    };

    // Peek at the version before trusting the rest of the shape: a file from a
    // newer build may have fields this one would reject.
    let version = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| v.get("version").and_then(serde_json::Value::as_u64));
    if let Some(v) = version {
        if v > u64::from(SCHEMA_VERSION) {
            let keep = path.with_file_name(format!("state.v{v}.json"));
            let moved = std::fs::rename(path, &keep).is_ok();
            let where_ = if moved { format!("; kept as {}", keep.display()) } else { String::new() };
            return (
                Persisted::default(),
                Some(format!("the state file is schema v{v} and this build understands v{SCHEMA_VERSION}{where_}")),
            );
        }
    }

    match serde_json::from_slice::<Persisted>(&bytes) {
        // A file with no version, or a version older than this build's, is a
        // migration. There is only one schema so far, so adopting it is the
        // whole migration; the next one adds a match here.
        Ok(mut p) => {
            let was = p.version;
            p.version = SCHEMA_VERSION;
            let note = (was != SCHEMA_VERSION)
                .then(|| format!("the state file was schema v{was}; migrated to v{SCHEMA_VERSION}"));
            (p, note)
        }
        Err(e) => {
            let bad = path.with_file_name(BAD_FILE);
            let moved = std::fs::rename(path, &bad).is_ok();
            let where_ = if moved { format!("; kept as {}", bad.display()) } else { String::new() };
            (Persisted::default(), Some(format!("the state file could not be parsed ({e}){where_}; starting from defaults")))
        }
    }
}

// ---------------------------------------------------------------- writing ---

struct Slot {
    /// The newest state waiting to be written. One slot: a burst of changes
    /// costs one write, and a save never waits for the disk.
    pending: Option<Persisted>,
    stop: bool,
    /// Bumped every time the writer finishes a pass, so `flush` can wait.
    passes: u64,
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
                    let (p, why) = load(&path);
                    if let Some(why) = &why {
                        eprintln!("studio: state: {why}");
                    }
                    health.recovered = why;
                    (Some(path), p)
                }
            }
        };

        let inner = Arc::new(Inner {
            path,
            slot: Mutex::new(Slot { pending: None, stop: false, passes: 0 }),
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
        let want = slot.passes + u64::from(slot.pending.is_some());
        while !slot.stop && slot.passes < want {
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
        let next = {
            let mut slot = inner.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            while slot.pending.is_none() && !slot.stop {
                slot = inner.wake.wait(slot).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            match slot.pending.take() {
                Some(p) => p,
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
                continue;
            }
        };
        // Nothing changed: do not touch the disk. A slider being dragged makes
        // one state change per frame and most of them are identical by the
        // time the writer gets here.
        if written.as_deref() == Some(text.as_str()) {
            finish_pass(inner);
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
        finish_pass(inner);
    }
    // Last wishes: whatever was asked for on the way out.
    let last = inner.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pending.take();
    if let Some(p) = last {
        if let Ok(t) = serde_json::to_string_pretty(&p) {
            let _ = write_atomically(&path, format!("{t}\n").as_bytes());
        }
    }
    finish_pass(inner);
}

fn finish_pass(inner: &Arc<Inner>) {
    inner.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).passes += 1;
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
