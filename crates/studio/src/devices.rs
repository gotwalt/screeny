//! The device registry: which panels this studio knows about.
//!
//! **A collection from day one**, even though one panel is the expected case
//! (`studio-vision.md`, decision 3). Nothing here is written as though there
//! were a single global device.
//!
//! Two ways in, merged into one list:
//!
//! - **discovery**: a periodic `_screeny._udp` browse. A convenience, not a
//!   requirement - Docker on macOS has no multicast at all, and avahi owns
//!   5353 on the Linux box - so the studio never depends on it;
//! - **manual**: a human typing a name or an address.
//!
//! **Keyed by the device's own stable id**, never by IP. A panel that takes a
//! new DHCP lease is the same panel with the same player; an address is a way
//! of reaching it, not a name for it. A manually added address is not known by
//! id until the studio has spoken to it, so it gets a *provisional* id
//! ([`PENDING`]`:<what was typed>`) and adopts its real one on the first
//! answer - the one moment when a device's key changes, and the one place that
//! has to be handled ([`Registry::resolved`] returns the rename so the player
//! can follow it).
//!
//! Bounded by construction: one browse at a time, one control request in
//! flight per device, a fixed telemetry period, and every fault logged once
//! per device rather than per attempt.

use screeny::proto::control::{state as dev_state, Telemetry};
use screeny::{ControlClient, Device, Target};
use serde::Serialize;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::state::{unix_now, StoredDevice};

/// Prefix of a provisional id, used until the device says what its real one is.
pub const PENDING: &str = "pending";
/// How long a control request may take before it is a failure. The client
/// retries three times inside this.
pub const CONTROL_TIMEOUT: Duration = Duration::from_millis(400);

/// How the studio reaches one device.
///
/// The distinction the brief asks for: a *name* a human typed is re-resolved
/// on every reconnect and so follows a DHCP lease; a device the *registry* has
/// resolved is attached to by its exact two ports and never browsed for.
#[derive(Clone, Debug)]
pub enum Reach {
    /// Resolved: attach to these ports (`Link::attach`, card 111).
    Resolved(Box<Device>),
    /// A DNS-SD instance name: `Link` re-resolves it on every reconnect.
    Name(String),
    /// A literal address.
    Addr(SocketAddr),
    /// Nothing to go on yet - a discovered device we have lost sight of, or a
    /// manual name we have never reached.
    Unknown,
}

/// A device as the studio knows it right now.
#[derive(Clone, Debug)]
pub struct DeviceRecord {
    pub stored: StoredDevice,
    /// The last resolution: exact addresses and ports.
    pub resolved: Option<Device>,
    /// When it was last seen, by discovery or by answering a control request.
    pub seen_unix: Option<u64>,
    /// The last telemetry heard, and when.
    pub telemetry: Option<Telem>,
    /// The last control request that failed, if the last one did.
    pub last_error: Option<String>,
}

impl DeviceRecord {
    /// A name for a human: what was set here, then the device's own, then the
    /// instance name, then the id.
    #[must_use]
    pub fn label(&self) -> String {
        if !self.stored.name.is_empty() {
            return self.stored.name.clone();
        }
        if let Some(n) = self.resolved.as_ref().and_then(|d| d.info.as_ref()).map(|i| i.name.clone()) {
            if !n.is_empty() {
                return n;
            }
        }
        if !self.stored.instance.is_empty() {
            return self.stored.instance.clone();
        }
        self.stored.id.clone()
    }

    /// How to reach it, best first.
    #[must_use]
    pub fn reach(&self) -> Reach {
        // A human-typed name wins over a stale resolution: re-resolving is how
        // a link follows the device across a DHCP lease.
        if let Some(d) = &self.resolved {
            return Reach::Resolved(Box::new(d.clone()));
        }
        if !self.stored.address.is_empty() {
            if let Some(a) = parse_addr(&self.stored.address) {
                return Reach::Addr(a);
            }
            return Reach::Name(self.stored.address.clone());
        }
        if !self.stored.instance.is_empty() {
            return Reach::Name(self.stored.instance.clone());
        }
        Reach::Unknown
    }

    /// The control port, when we know it.
    #[must_use]
    pub fn control_addr(&self) -> Option<SocketAddr> {
        self.resolved.as_ref().map(|d| d.control)
    }

    /// True when the device has been heard from recently.
    #[must_use]
    pub fn online(&self, within_s: u64) -> bool {
        self.seen_unix.is_some_and(|s| unix_now().saturating_sub(s) <= within_s)
    }
}

/// What the device says about itself (spec 6.7), in the shape the dashboard
/// puts on screen.
#[derive(Clone, Debug, Serialize)]
pub struct Telem {
    /// When this was heard, so the UI can say "3 s ago" or "never".
    pub heard_unix: u64,
    pub uptime_s: u64,
    pub rssi_dbm: i8,
    pub brightness: u8,
    pub state: &'static str,
    pub frames_rx: u32,
    pub frames_shown: u32,
    pub drops: Drops,
    pub interarrival_us: u16,
    pub jitter_us: u16,
    pub decode_us: u16,
}

/// The four drop counters, by cause, plus the gaps the device noticed.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Drops {
    pub stale: u32,
    pub superseded: u32,
    pub decode: u32,
    pub rejected: u32,
    pub seq_gaps: u32,
}

impl Telem {
    #[must_use]
    pub fn of(t: &Telemetry) -> Self {
        Telem {
            heard_unix: unix_now(),
            uptime_s: u64::from(t.uptime_ms) / 1000,
            rssi_dbm: t.rssi_dbm,
            brightness: t.brightness,
            state: state_name(t.state),
            frames_rx: t.frames_rx,
            frames_shown: t.frames_shown,
            drops: Drops {
                stale: t.frames_dropped_stale,
                superseded: t.frames_dropped_superseded,
                decode: t.frames_dropped_decode,
                rejected: t.frames_rejected,
                seq_gaps: t.seq_gaps,
            },
            interarrival_us: t.interarrival_us,
            jitter_us: t.jitter_us,
            decode_us: t.decode_us,
        }
    }
}

fn state_name(s: u8) -> &'static str {
    match s {
        dev_state::IDLE => "idle",
        dev_state::LIVE => "live",
        dev_state::HOLD => "hold",
        dev_state::IDENTIFY => "identify",
        dev_state::PROVISIONING => "provisioning",
        _ => "unknown",
    }
}

/// How a discovery browse is going, for the status route. A browse that finds
/// nothing is normal, not an error (`crates/screeny/README.md`).
#[derive(Clone, Debug, Default, Serialize)]
pub struct DiscoveryHealth {
    pub enabled: bool,
    pub browses: u64,
    pub last_browse_unix: Option<u64>,
    pub last_found: usize,
    /// The last browse that failed. Not a reason to be unhealthy: the studio
    /// works from configured addresses alone, by design.
    pub last_error: Option<String>,
}

/// Every device the studio knows about.
#[derive(Default)]
pub struct Registry {
    devices: Mutex<BTreeMap<String, DeviceRecord>>,
    discovery: Mutex<DiscoveryHealth>,
}

/// What changed, so the caller can move a player and persist.
#[derive(Clone, Debug, Default)]
pub struct Changes {
    /// Devices whose key changed when their real id arrived: `(old, new)`.
    pub renamed: Vec<(String, String)>,
    /// Devices that appeared.
    pub added: Vec<String>,
}

impl Registry {
    #[must_use]
    pub fn new() -> Self {
        Registry::default()
    }

    /// Adopt what was in the state file.
    pub fn load(&self, stored: Vec<StoredDevice>) {
        let mut devices = self.lock();
        for s in stored {
            if s.id.is_empty() {
                continue;
            }
            devices.insert(
                s.id.clone(),
                DeviceRecord { stored: s, resolved: None, seen_unix: None, telemetry: None, last_error: None },
            );
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, DeviceRecord>> {
        self.devices.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn list(&self) -> Vec<DeviceRecord> {
        self.lock().values().cloned().collect()
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<DeviceRecord> {
        self.lock().get(id).cloned()
    }

    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }

    /// What to write to the state file.
    #[must_use]
    pub fn stored(&self) -> Vec<StoredDevice> {
        self.lock().values().map(|d| d.stored.clone()).collect()
    }

    #[must_use]
    pub fn discovery_health(&self) -> DiscoveryHealth {
        self.discovery.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn set_discovery_enabled(&self, on: bool) {
        self.discovery.lock().unwrap_or_else(std::sync::PoisonError::into_inner).enabled = on;
    }

    /// Add a device a human typed in.
    ///
    /// `to` is an `IP[:PORT]` or a DNS-SD instance name. The id is provisional
    /// until the device answers; [`Registry::resolved`] replaces it then.
    ///
    /// # Errors
    ///
    /// If `to` is empty.
    pub fn add_manual(&self, to: &str, name: &str) -> Result<String, String> {
        let to = to.trim();
        if to.is_empty() {
            return Err("give a name or an address".into());
        }
        let mut devices = self.lock();
        // Already known by that address or instance name? Then this is a
        // rename, not a second device.
        if let Some(existing) = devices.values_mut().find(|d| d.stored.address == to || d.stored.instance == to) {
            existing.stored.manual = true;
            if !name.is_empty() {
                existing.stored.name = name.to_string();
            }
            if existing.stored.address.is_empty() {
                existing.stored.address = to.to_string();
            }
            return Ok(existing.stored.id.clone());
        }
        let id = format!("{PENDING}:{to}");
        let stored = StoredDevice {
            id: id.clone(),
            name: name.to_string(),
            instance: if parse_addr(to).is_some() { String::new() } else { to.to_string() },
            address: to.to_string(),
            manual: true,
        };
        devices.insert(id.clone(), DeviceRecord { stored, resolved: None, seen_unix: None, telemetry: None, last_error: None });
        Ok(id)
    }

    /// Forget a device entirely. The caller stops its player.
    pub fn forget(&self, id: &str) -> bool {
        self.lock().remove(id).is_some()
    }

    /// What this studio calls it. Empty means "use the device's own name".
    pub fn rename(&self, id: &str, name: &str) -> bool {
        match self.lock().get_mut(id) {
            Some(d) => {
                d.stored.name = name.trim().to_string();
                true
            }
            None => false,
        }
    }

    /// A device answered, or a browse found it. Merge it in, adopting its real
    /// id if this is the first time we have heard it.
    ///
    /// Returns `(id, renamed_from)`.
    pub fn resolved(&self, dev: &Device) -> (String, Option<String>) {
        let real = dev.info.as_ref().map(|i| i.id.clone()).filter(|i| !i.is_empty());
        let mut devices = self.lock();

        // The key it should have: its own id, or a provisional one built from
        // its instance name so that a device seen twice is one device.
        let key = real.clone().unwrap_or_else(|| format!("{PENDING}:{}", dev.instance));

        // Which record is this, if any? By real id, then by the provisional id
        // a human's typing made, then by instance name, then by address.
        let typed_addr = dev.frame.to_string();
        let ip_only = dev.frame.ip().to_string();
        let old_key = devices
            .keys()
            .find(|k| **k == key)
            .or_else(|| {
                devices
                    .iter()
                    .find(|(_, d)| {
                        (!d.stored.instance.is_empty() && d.stored.instance == dev.instance)
                            || (!d.stored.address.is_empty()
                                && (d.stored.address == typed_addr || d.stored.address == ip_only || d.stored.address == dev.instance))
                    })
                    .map(|(k, _)| k)
            })
            .cloned();

        let mut record = match &old_key {
            Some(k) => devices.remove(k).unwrap_or_default(),
            None => DeviceRecord::default(),
        };
        record.stored.id = key.clone();
        if record.stored.instance.is_empty() && !dev.instance.is_empty() && parse_addr(&dev.instance).is_none() {
            record.stored.instance = dev.instance.clone();
        }
        record.resolved = Some(dev.clone());
        record.seen_unix = Some(unix_now());
        record.last_error = None;
        devices.insert(key.clone(), record);

        let renamed = old_key.filter(|k| *k != key);
        (key, renamed)
    }

    /// Record telemetry heard from a device.
    pub fn heard(&self, id: &str, t: &Telemetry) {
        if let Some(d) = self.lock().get_mut(id) {
            d.telemetry = Some(Telem::of(t));
            d.seen_unix = Some(unix_now());
            d.last_error = None;
        }
    }

    /// Record that a control request failed. Logged by the caller, once.
    pub fn control_failed(&self, id: &str, why: String) {
        if let Some(d) = self.lock().get_mut(id) {
            d.last_error = Some(why);
        }
    }

    /// A browse finished. `found` is every device it saw.
    pub fn browsed(&self, found: &[Device], error: Option<String>) -> Changes {
        {
            let mut h = self.discovery.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            h.browses += 1;
            h.last_browse_unix = Some(unix_now());
            h.last_found = found.len();
            if let Some(e) = &error {
                if h.last_error.as_deref() != Some(e.as_str()) {
                    eprintln!("studio: discovery: {e}");
                }
            }
            h.last_error = error;
        }
        let mut changes = Changes::default();
        for dev in found {
            let known = self.lock().contains_key(&dev.info.as_ref().map_or_else(String::new, |i| i.id.clone()));
            let (id, renamed) = self.resolved(dev);
            if let Some(from) = renamed {
                changes.renamed.push((from, id.clone()));
            } else if !known {
                changes.added.push(id);
            }
        }
        changes
    }
}

impl Default for DeviceRecord {
    fn default() -> Self {
        DeviceRecord { stored: StoredDevice::default(), resolved: None, seen_unix: None, telemetry: None, last_error: None }
    }
}

/// `IP`, `IP:PORT` or `[v6]:PORT`; anything else is a name.
#[must_use]
pub fn parse_addr(s: &str) -> Option<SocketAddr> {
    let s = s.trim();
    if let Ok(a) = s.parse::<SocketAddr>() {
        return Some(a);
    }
    s.parse::<std::net::IpAddr>().ok().map(|ip| SocketAddr::new(ip, screeny::proto::DEFAULT_FRAME_PORT))
}

// ------------------------------------------------------------- the wire ----

/// One `GET_INFO` on a control address: who is this, really?
///
/// Blocking, and short - the client times out in 250 ms and tries three times.
/// Call it from `spawn_blocking`.
///
/// # Errors
///
/// If the device does not answer.
pub fn identify_at(frame: SocketAddr) -> Result<Device, String> {
    let mut dev = Device::from_addr(frame);
    let mut client = ControlClient::connect(dev.control).map_err(|e| e.to_string())?;
    client.set_timeout(CONTROL_TIMEOUT);
    let info = client.info().map_err(|e| e.to_string())?;
    dev.apply(info);
    Ok(dev)
}

/// One browse of `_screeny._udp`.
///
/// # Errors
///
/// If mDNS itself fails. Finding nothing is `Ok(vec![])`, not an error.
pub fn browse(timeout: Duration) -> Result<Vec<Device>, String> {
    screeny::discover::browse(timeout, None).map_err(|e| e.to_string())
}

/// Resolve a DNS-SD instance name to a device.
///
/// # Errors
///
/// If nothing with that name answers in `timeout`.
pub fn resolve_name(name: &str, timeout: Duration) -> Result<Device, String> {
    Target { name: Some(name.to_string()), timeout: Some(timeout), ..Target::default() }
        .resolve()
        .map_err(|e| e.to_string())
}

/// A control client pointed at a device, with this crate's timeout.
///
/// # Errors
///
/// If the socket cannot be opened.
pub fn control(addr: SocketAddr) -> Result<ControlClient, String> {
    let mut c = ControlClient::connect(addr).map_err(|e| e.to_string())?;
    c.set_timeout(CONTROL_TIMEOUT);
    Ok(c)
}

/// The moment used for "how long ago", in the same units everywhere.
#[must_use]
pub fn since(then: Instant) -> f64 {
    then.elapsed().as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use screeny::DeviceInfo;

    fn device(instance: &str, id: &str, addr: &str) -> Device {
        let mut d = Device::from_addr(addr.parse().expect("an address"));
        d.instance = instance.to_string();
        d.apply(DeviceInfo { id: id.to_string(), name: String::new(), ctrl: d.control.port(), ..info() });
        d
    }

    fn info() -> DeviceInfo {
        DeviceInfo {
            txtvers: 1,
            proto: "1".into(),
            w: 64,
            h: 32,
            codecs: vec![0x10],
            codecs_raw: "10".into(),
            mtu: 1464,
            ctrl: 49375,
            fw: "test".into(),
            id: String::new(),
            name: String::new(),
        }
    }

    #[test]
    fn a_manual_address_gets_a_provisional_id_and_adopts_its_real_one() {
        let reg = Registry::new();
        let id = reg.add_manual("192.0.2.7:49374", "desk").expect("added");
        assert!(id.starts_with(PENDING), "{id}");
        assert_eq!(reg.list().len(), 1);

        // The device answers: the record keeps its name and its manual flag,
        // and its key becomes the device's own id.
        let (new_id, renamed) = reg.resolved(&device("screeny-abc", "abc123", "192.0.2.7:49374"));
        assert_eq!(new_id, "abc123");
        assert_eq!(renamed.as_deref(), Some(id.as_str()));
        assert_eq!(reg.list().len(), 1, "adopting an id must not make a second device");
        let d = reg.get("abc123").expect("still there");
        assert_eq!(d.stored.name, "desk");
        assert!(d.stored.manual);
        assert_eq!(d.stored.instance, "screeny-abc");
        assert_eq!(d.label(), "desk");
    }

    /// The property the whole registry is for: a device that moves address is
    /// the same device, with the same player.
    #[test]
    fn a_device_that_changes_address_keeps_its_id() {
        let reg = Registry::new();
        let (id, _) = reg.resolved(&device("screeny-abc", "abc123", "192.0.2.7:49374"));
        let (again, renamed) = reg.resolved(&device("screeny-abc", "abc123", "192.0.2.99:49374"));
        assert_eq!(id, again);
        assert!(renamed.is_none());
        assert_eq!(reg.list().len(), 1);
        assert_eq!(reg.get("abc123").expect("there").resolved.expect("resolved").frame.to_string(), "192.0.2.99:49374");
    }

    #[test]
    fn a_device_with_no_id_is_keyed_by_its_instance_name() {
        let reg = Registry::new();
        let mut d = Device::from_addr("192.0.2.7:49374".parse().expect("addr"));
        d.instance = "screeny-noid".into();
        let (id, _) = reg.resolved(&d);
        assert_eq!(id, format!("{PENDING}:screeny-noid"));
    }

    #[test]
    fn reach_prefers_a_resolution_then_what_was_typed() {
        let reg = Registry::new();
        reg.add_manual("screeny-abc", "").expect("added");
        let rec = reg.list().remove(0);
        assert!(matches!(rec.reach(), Reach::Name(n) if n == "screeny-abc"));

        reg.add_manual("192.0.2.7", "").expect("added");
        let rec = reg.get(&format!("{PENDING}:192.0.2.7")).expect("there");
        assert!(matches!(rec.reach(), Reach::Addr(a) if a.to_string() == "192.0.2.7:49374"));

        reg.resolved(&device("screeny-abc", "abc123", "192.0.2.7:49374"));
        let rec = reg.get("abc123").expect("there");
        assert!(matches!(rec.reach(), Reach::Resolved(_)));
    }

    #[test]
    fn adding_the_same_address_twice_is_one_device() {
        let reg = Registry::new();
        let a = reg.add_manual("192.0.2.7", "one").expect("added");
        let b = reg.add_manual("192.0.2.7", "two").expect("added");
        assert_eq!(a, b);
        assert_eq!(reg.list().len(), 1);
        assert_eq!(reg.get(&a).expect("there").stored.name, "two");
        assert!(reg.add_manual("   ", "").is_err());
    }

    #[test]
    fn what_is_stored_survives_a_round_trip() {
        let reg = Registry::new();
        reg.add_manual("192.0.2.7", "desk").expect("added");
        reg.resolved(&device("screeny-abc", "abc123", "192.0.2.7:49374"));
        let stored = reg.stored();
        assert_eq!(stored.len(), 1);

        let back = Registry::new();
        back.load(stored);
        let d = back.get("abc123").expect("loaded");
        assert_eq!(d.stored.name, "desk");
        assert_eq!(d.stored.instance, "screeny-abc");
        // A resolution is deliberately not persisted: an address from last
        // month is worse than no address at all.
        assert!(d.resolved.is_none());
        assert!(matches!(d.reach(), Reach::Addr(_)));
    }
}
