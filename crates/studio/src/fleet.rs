//! The three background tasks that make the studio worth leaving alone: the
//! supervisor, the discovery browse and the telemetry poll.
//!
//! All three are *slow* loops - once a second, once every thirty seconds, once
//! every five - and all three are bounded: one browse at a time, one control
//! request at a time, one pass over a list whose length is the number of panels
//! in the house. Anything that talks to the network does so on a blocking
//! thread, because a UDP round trip with retries takes over a second in the bad
//! case and a runtime worker has better things to do.
//!
//! Faults back off. A device that does not answer is asked less and less often,
//! up to a cap, with a little jitter so several devices do not fall into step -
//! that is the difference between "a panel is off for a month" and "a retry
//! storm for a month".

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::devices::{self, Reach, PENDING};
use crate::player::{BrightnessJob, Player, PlayerChange};
use crate::state::{unix_now, StoredPlayer, UNBOUND};
use crate::AppState;

/// The longest a failing device's poll is backed off to, as a multiple of the
/// telemetry period.
const MAX_BACKOFF: u32 = 12;

// ----------------------------------------------------------- attachment ----

/// **Attach the page to a panel**, and hand back the player that drives it.
///
/// This is the one place a panel becomes "the panel this studio is connected
/// to", and the rule that matters is the middle branch: if the page is showing
/// the *unbound* player - a studio that has not met a panel yet - that same
/// player is renamed onto the device. The thread, the core and the piece carry
/// on, so the picture the browser is watching simply starts reaching the
/// panel instead of restarting on it.
pub fn attach(st: &AppState, device: &str) -> Arc<Player> {
    if let Some(p) = st.players.get(device) {
        st.players.set_focus(device);
        return p;
    }
    if st.players.get(UNBOUND).is_some() {
        st.players.rekey(UNBOUND, device);
    } else {
        st.players.ensure(device, st.cfg.fault_pieces, StoredPlayer::default);
    }
    st.players.set_focus(device);
    st.players.get(device).unwrap_or_else(|| st.page())
}

/// The player for one device, for a caller naming a panel rather than asking
/// for the page's.
///
/// A studio that has never been attached to anything treats this as the
/// attachment - it is the first panel somebody has named, and leaving the page
/// showing an unbound player beside it would be two pictures and a puzzle.
/// Once there is an attached panel, this simply makes a second player.
pub fn player_for(st: &AppState, device: &str) -> Arc<Player> {
    if st.players.bound().is_empty() {
        return attach(st, device);
    }
    st.players.ensure(device, st.cfg.fault_pieces, StoredPlayer::default)
}

/// Point a player's link at wherever its device is now, or at nothing.
pub fn aim_at_device(st: &AppState, player: &Arc<Player>) {
    let reach = st.devices.get(&player.device()).map_or(Reach::Unknown, |r| r.reach());
    player.aim(&reach);
}

/// The watchdog, the aim of every link, and the brightness policy. Once a
/// second.
pub fn spawn_supervisor(st: AppState) {
    let mut stop = st.stop.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(st.cfg.supervise_every);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // If the state file already had a panel, the studio is not fresh and
        // must not adopt anything on its own.
        let mut adopted = !st.players.bound().is_empty();
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if !adopted {
                        adopted = adopt_first_device(&st);
                    }
                    supervise(&st).await;
                }
                // The borrow `wait_for` hands back is not `Send` and this
                // future has to be: discard it inside the block.
                () = async { drop(stop.wait_for(|s| *s).await) } => break,
            }
        }
    });
}

/// A studio that has never been attached to a panel attaches itself to the
/// first one it finds. That is the zero-click case the owner asked for: plug
/// the panel in, open the page, and the picture the page was already showing
/// is on the panel.
///
/// It happens **once** per process: a human who detaches the only panel has
/// detached it.
fn adopt_first_device(st: &AppState) -> bool {
    if !st.players.bound().is_empty() {
        return true;
    }
    let Some(first) = st.devices.ids().into_iter().next() else { return false };
    let player = attach(st, &first);
    let _ = player.configure(&PlayerChange { on: Some(true), ..PlayerChange::default() });
    aim_at_device(st, &player);
    player.ensure_running();
    eprintln!("studio: attaching to the first panel found: `{}` will play `{}`", first, player.stored().piece);
    st.persist();
    true
}

async fn supervise(st: &AppState) {
    let known = st.devices.ids();
    // A player whose device has been forgotten goes with it. The unbound
    // player is nobody's device and stays.
    for player in st.players.all() {
        let device = player.device();
        if device != UNBOUND && !known.contains(&device) {
            st.players.remove(&device);
        }
    }
    // The page always has something to show, whatever just happened.
    st.players.ensure_page(st.cfg.fault_pieces);

    let mut jobs: Vec<BrightnessJob> = Vec::new();
    for player in st.players.all() {
        aim_at_device(st, &player);
        let device = player.device();
        match player.supervise() {
            Some(job) => jobs.push(job),
            // The link noticing a new session is not the only way a panel
            // comes back at its own brightness: a power cut short enough that
            // UDP never noticed leaves the link perfectly happy and the panel
            // at full. The device's own telemetry is the honest check, and
            // comparing against what it *said it applied* - not what was asked
            // for - is what stops this retrying for ever against a cap.
            None if device != UNBOUND => {
                if let (Some(want), Some(applied)) = player.brightness_policy() {
                    let heard = st
                        .devices
                        .get(&device)
                        .and_then(|d| d.telemetry)
                        .filter(|t| unix_now().saturating_sub(t.heard_unix) <= 30);
                    if heard.is_some_and(|t| t.brightness != applied) {
                        jobs.push(BrightnessJob { device, level: want });
                    }
                }
            }
            None => {}
        }
    }

    // Brightness is a control request: off this task, and never on a render
    // thread, where a second of waiting would trip the watchdog.
    for job in jobs {
        apply_brightness(st, &job).await;
    }
}

/// Apply the brightness policy, and record what the device actually did with
/// it - the firmware caps brightness and the reply says where the cap is.
async fn apply_brightness(st: &AppState, job: &BrightnessJob) {
    let Some(addr) = st.devices.get(&job.device).and_then(|d| d.control_addr()) else { return };
    let level = job.level;
    let done = tokio::task::spawn_blocking(move || devices::control(addr).and_then(|mut c| c.set_brightness(level).map_err(|e| e.to_string()))).await;
    match done {
        Ok(Ok(applied)) => {
            if let Some(p) = st.players.get(&job.device) {
                p.brightness_applied(level, applied);
            }
            if applied != level {
                eprintln!("studio: {}: asked for brightness {level}, the device applied {applied}", job.device);
            }
        }
        Ok(Err(e)) => st.devices.control_failed(&job.device, format!("setting brightness: {e}")),
        Err(e) => eprintln!("studio: {}: setting brightness: {e}", job.device),
    }
}

/// Browse `_screeny._udp`, if discovery is on at all.
///
/// Finding nothing is normal, not an error; so is mDNS failing outright, which
/// is a Tuesday inside Docker on macOS. Everything still works from configured
/// addresses, which is why this task is allowed to be a convenience.
pub fn spawn_discovery(st: AppState) {
    if !st.cfg.discover {
        return;
    }
    let mut stop = st.stop.clone();
    tokio::spawn(async move {
        let timeout = Duration::from_secs(3).min(st.cfg.discover_every);
        let mut ticker = tokio::time::interval(st.cfg.discover_every);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let found = tokio::task::spawn_blocking(move || devices::browse(timeout)).await;
                    let (list, err) = match found {
                        Ok(Ok(list)) => (list, None),
                        Ok(Err(e)) => (Vec::new(), Some(e)),
                        Err(e) => (Vec::new(), Some(e.to_string())),
                    };
                    let changes = st.devices.browsed(&list, err);
                    for (from, to) in &changes.renamed {
                        st.players.rekey(from, to);
                    }
                    if !changes.renamed.is_empty() || !changes.added.is_empty() {
                        for id in &changes.added {
                            eprintln!("studio: found `{id}`");
                        }
                        st.persist();
                    }
                }
                // The borrow `wait_for` hands back is not `Send` and this
                // future has to be: discard it inside the block.
                () = async { drop(stop.wait_for(|s| *s).await) } => break,
            }
        }
    });
}

/// Ask each device how it is - and, for a manually typed address, who it is.
///
/// **This is the one place the studio asks a device about itself.** Today that
/// is `TELEMETRY` on the control port (spec 6.7); when the firmware grows an
/// HTTP status API, this function is what changes and nothing else does.
pub fn spawn_telemetry(st: AppState) {
    let mut stop = st.stop.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(st.cfg.telemetry_every);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Per device: how many passes to skip, and how many are left.
        let mut backoff: BTreeMap<String, (u32, u32)> = BTreeMap::new();
        loop {
            tokio::select! {
                _ = ticker.tick() => poll_once(&st, &mut backoff).await,
                // The borrow `wait_for` hands back is not `Send` and this
                // future has to be: discard it inside the block.
                () = async { drop(stop.wait_for(|s| *s).await) } => break,
            }
        }
    });
}

async fn poll_once(st: &AppState, backoff: &mut BTreeMap<String, (u32, u32)>) {
    let devices_now = st.devices.list();
    backoff.retain(|id, _| devices_now.iter().any(|d| d.stored.id == *id));

    for record in devices_now {
        let id = record.stored.id.clone();

        // A resolution we have not been able to confirm for a long time is a
        // guess about last hour's network. Throw it away and find out again.
        let unheard = record.seen_unix.map_or(u64::MAX, |s| unix_now().saturating_sub(s));
        if record.resolved.is_some() && unheard > st.cfg.stale_after.as_secs() {
            st.devices.stale(&id);
            continue;
        }
        if let Some((_, left)) = backoff.get_mut(&id) {
            if *left > 0 {
                *left -= 1;
                continue;
            }
        }

        // A device whose exact ports we do not know: ask it who it is.
        //
        // Two cases, and the second is the one that matters after a restart.
        // A *pending* id has never been reached at all. But a device read back
        // out of the state file has its real id and no resolution - an address
        // from last month is worse than no address - so it needs the same
        // `GET_INFO` before there is a control port to talk to.
        if record.resolved.is_none() {
            if !id.starts_with(PENDING) && record.stored.address.is_empty() {
                // Known by instance name only: discovery is the way in.
                fail(backoff, &id);
                continue;
            }
            let Some(addr) = devices::parse_addr(&record.stored.address) else {
                // A name, and discovery is the only way to resolve one.
                fail(backoff, &id);
                continue;
            };
            match tokio::task::spawn_blocking(move || devices::identify_at(addr)).await {
                Ok(Ok(dev)) => {
                    let (new_id, renamed) = st.devices.resolved(&dev);
                    if let Some(from) = renamed {
                        st.players.rekey(&from, &new_id);
                        eprintln!("studio: `{from}` is `{new_id}` ({})", dev.label());
                    }
                    backoff.remove(&id);
                    st.persist();
                }
                Ok(Err(e)) => {
                    st.devices.control_failed(&id, e);
                    fail(backoff, &id);
                }
                Err(e) => {
                    st.devices.control_failed(&id, e.to_string());
                    fail(backoff, &id);
                }
            }
            continue;
        }

        let Some(addr) = record.control_addr() else {
            fail(backoff, &id);
            continue;
        };
        match tokio::task::spawn_blocking(move || devices::control(addr).and_then(|mut c| c.telemetry().map_err(|e| e.to_string()))).await {
            Ok(Ok(t)) => {
                st.devices.heard(&id, &t);
                backoff.remove(&id);
            }
            Ok(Err(e)) => {
                st.devices.control_failed(&id, format!("asking for telemetry: {e}"));
                fail(backoff, &id);
            }
            Err(e) => {
                st.devices.control_failed(&id, e.to_string());
                fail(backoff, &id);
            }
        }
    }
}

// ------------------------------------------- the device's own HTTP status ----

/// **Read each device's own `GET /api/v1/status`** - the things only the
/// device knows: heap, free stack, firmware slot, reset reason, WiFi.
///
/// This is the other half of the seam card 106 left: [`spawn_telemetry`] above
/// still owns the frame counters and the link, and this adds to them rather
/// than replacing them. Absent or failing HTTP is normal and never a problem
/// in `/healthz`.
///
/// **Every rule the firmware session asked for is a property of this one task**
/// (card 222: the device has one connection worker and no listen backlog, so a
/// second simultaneous connection is dropped at SYN):
///
/// * *one poller* - one task, spawned once;
/// * *one connection at a time* - the loop `await`s each read before starting
///   the next, so there is one in flight across the whole fleet, not merely
///   one per device;
/// * *no faster than every 10 s* - [`crate::MIN_DEVICE_HTTP_EVERY`], which is
///   what `Config::default` carries and what `main` clamps to;
/// * *`Connection: close`, ~2 s, bounded reply* - [`crate::devhttp`];
/// * *capped jittered backoff* - [`fail`], the same one the telemetry poll uses;
/// * *never on a render thread* - `spawn_blocking`;
/// * *never triggered by a browser* - no route calls this. A browser reads the
///   studio's cached copy through `/api/v1/status`.
pub fn spawn_device_http(st: AppState) {
    if !st.cfg.device_http {
        return;
    }
    let mut stop = st.stop.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(st.cfg.device_http_every);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut backoff: BTreeMap<String, (u32, u32)> = BTreeMap::new();
        loop {
            tokio::select! {
                _ = ticker.tick() => status_once(&st, &mut backoff).await,
                // The borrow `wait_for` hands back is not `Send` and this
                // future has to be: discard it inside the block.
                () = async { drop(stop.wait_for(|s| *s).await) } => break,
            }
        }
    });
}

async fn status_once(st: &AppState, backoff: &mut BTreeMap<String, (u32, u32)>) {
    let devices_now = st.devices.list();
    backoff.retain(|id, _| devices_now.iter().any(|d| d.stored.id == *id));

    for record in devices_now {
        let id = record.stored.id.clone();
        if let Some((_, left)) = backoff.get_mut(&id) {
            if *left > 0 {
                *left -= 1;
                continue;
            }
        }
        // Nowhere to ask yet. Not a failure and not worth backing off for:
        // the telemetry poll is what finds out where this device is.
        let Some(addr) = record.http_addr(st.cfg.device_http_port) else { continue };

        let read = tokio::task::spawn_blocking(move || crate::devhttp::get_status(addr, crate::devhttp::TIMEOUT)).await;
        match read {
            Ok(Ok(reply)) => {
                let first = record.http.reads == 0;
                // The reply is handed straight to the registry and never held
                // here: it carries the SSID, and nothing below may print it.
                let fw = reply.fw.to_string();
                let slot = format!("{:?}", reply.fw_slot).to_lowercase();
                if let Some((reboots, rebooted)) = st.devices.heard_http(&id, reply) {
                    if first {
                        eprintln!("studio: `{}` serves its own status API: firmware {fw}, slot {slot}", record.label());
                    }
                    if rebooted {
                        eprintln!("studio: `{}` rebooted: {reboots} since the studio started", record.label());
                    }
                }
                backoff.remove(&id);
            }
            Ok(Err(fault)) => {
                if st.devices.http_failed(&id, fault.absent, fault.why.clone()) {
                    if fault.absent {
                        eprintln!(
                            "studio: `{}` has no HTTP status API ({fault}); its UDP telemetry is all the studio will show",
                            record.label()
                        );
                    } else {
                        eprintln!("studio: `{}`: reading its status: {fault}", record.label());
                    }
                }
                // A device with no server is asked at the slowest rate at
                // once rather than climbing to it: a firmware update is the
                // only thing that changes the answer, and that is not a thing
                // that happens twice a minute.
                if fault.absent {
                    backoff.insert(id, (MAX_BACKOFF, MAX_BACKOFF));
                } else {
                    fail(backoff, &id);
                }
            }
            Err(e) => {
                st.devices.http_failed(&id, false, e.to_string());
                fail(backoff, &id);
            }
        }
    }
}

/// Capped exponential backoff with a little jitter, so a house full of panels
/// that are all off does not poll in lockstep.
fn fail(backoff: &mut BTreeMap<String, (u32, u32)>, id: &str) {
    let entry = backoff.entry(id.to_string()).or_insert((0, 0));
    entry.0 = entry.0.saturating_mul(2).clamp(1, MAX_BACKOFF);
    // Jitter: 0 or 1 extra pass, from the clock rather than a random number
    // generator this crate does not otherwise need.
    let jitter = u32::from(crate::player::unix_millis().is_multiple_of(2));
    entry.1 = entry.0 + jitter;
}
