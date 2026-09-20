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
use std::time::Duration;

use crate::devices::{self, PENDING};
use crate::player::BrightnessJob;
use crate::state::{unix_now, StoredPlayer};
use crate::AppState;

/// The longest a failing device's poll is backed off to, as a multiple of the
/// telemetry period.
const MAX_BACKOFF: u32 = 12;

/// The watchdog, the aim of every link, and the brightness policy. Once a
/// second.
pub fn spawn_supervisor(st: AppState) {
    let mut stop = st.stop.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(st.cfg.supervise_every);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // If the state file already had players, the studio is not fresh and
        // must not adopt anything on its own.
        let mut adopted = !st.players.ids().is_empty();
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

/// A fresh studio with no state file plays something on the first panel it
/// finds. That is the card's "sane default", and it happens **once** per
/// process: a human who deletes the only player has deleted it.
fn adopt_first_device(st: &AppState) -> bool {
    let Some(first) = st.devices.ids().into_iter().next() else { return false };
    let faults = st.cfg.fault_pieces;
    let p = st.players.ensure(&first, faults, StoredPlayer::default);
    eprintln!("studio: no player was configured; `{}` will play `{}`", first, p.stored().piece);
    st.persist();
    true
}

async fn supervise(st: &AppState) {
    let known = st.devices.ids();
    // A player whose device has been forgotten goes with it.
    for id in st.players.ids() {
        if !known.contains(&id) {
            st.players.remove(&id);
        }
    }

    let mut jobs: Vec<BrightnessJob> = Vec::new();
    for id in &known {
        let Some(player) = st.players.get(id) else { continue };
        if let Some(record) = st.devices.get(id) {
            player.aim(&record.reach());
        }
        match player.supervise() {
            Some(job) => jobs.push(job),
            // The link noticing a new session is not the only way a panel
            // comes back at its own brightness: a power cut short enough that
            // UDP never noticed leaves the link perfectly happy and the panel
            // at full. The device's own telemetry is the honest check, and
            // comparing against what it *said it applied* - not what was asked
            // for - is what stops this retrying for ever against a cap.
            None => {
                if let (Some(want), Some(applied)) = player.brightness_policy() {
                    let heard = st.devices.get(id).and_then(|d| d.telemetry).filter(|t| unix_now().saturating_sub(t.heard_unix) <= 30);
                    if heard.is_some_and(|t| t.brightness != applied) {
                        jobs.push(BrightnessJob { device: id.clone(), level: want });
                    }
                }
            }
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
