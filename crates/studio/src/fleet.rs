//! The four background tasks that make the studio worth leaving alone: the
//! supervisor, the discovery browse, the telemetry poll and - card 180 - the
//! read of each device's own HTTP status API.
//!
//! All four are *slow* loops - once a second, once every thirty seconds, once
//! every five, once every ten - and all four are bounded: one browse at a time,
//! one control request at a time, one HTTP connection at a time across the
//! whole fleet, one pass over a list whose length is the number of panels
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
/// player is renamed onto the device. The thread, the core and the patch carry
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
        st.players.ensure(device, st.cfg.fault_patches, StoredPlayer::default);
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
    st.players.ensure(device, st.cfg.fault_patches, StoredPlayer::default)
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
    eprintln!("studio: attaching to the first panel found: `{}` will play `{}`", first, player.stored().patch);
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
    st.players.ensure_page(st.cfg.fault_patches);

    let mut jobs: Vec<BrightnessJob> = Vec::new();
    for player in st.players.all() {
        aim_at_device(st, &player);
        let device = player.device();
        // Card 164: the frame path's counters, read once a second off the link
        // the supervisor is already holding. The render loop does not know
        // this exists.
        if device != UNBOUND {
            st.devices.metered_link(&device, player.link_traffic());
        }
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

    // **Card 164: the rate is worked out here and nowhere else.** One pass,
    // after everything this tick has added, so every browser reading
    // `/api/v1/status` gets the same number and no page has to divide two
    // counters it fetched at two different moments.
    st.devices.sample_traffic();
}

/// Apply the brightness policy, and record what the device actually did with
/// it - the firmware caps brightness and the reply says where the cap is.
async fn apply_brightness(st: &AppState, job: &BrightnessJob) {
    let Some(addr) = st.devices.get(&job.device).and_then(|d| d.control_addr()) else { return };
    let level = job.level;
    let done = tokio::task::spawn_blocking(move || {
        devices::control_call(addr, |c| c.set_brightness(level).map_err(|e| e.to_string()))
    })
    .await;
    let done = match done {
        Ok((cost, out)) => {
            st.devices.metered_control(&job.device, cost);
            Ok(out)
        }
        Err(e) => Err(e),
    };
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

/// Browse `_screeny._udp`, and probe for a panel that has moved, if discovery
/// is on at all.
///
/// Finding nothing is normal, not an error; so is mDNS failing outright, which
/// is a Tuesday inside Docker on macOS. Everything still works from configured
/// addresses, which is why this task is allowed to be a convenience.
///
/// The two live on **one** tick, in this order, and the probe is the second
/// one: a browse is never delayed by a probe, and there is one of each in
/// flight at a time by construction, because this loop awaits them.
pub fn spawn_discovery(st: AppState) {
    if !st.cfg.discover && !probing(&st) {
        return;
    }
    let mut stop = st.stop.clone();
    tokio::spawn(async move {
        let timeout = Duration::from_secs(3).min(st.cfg.discover_every);
        let mut ticker = tokio::time::interval(st.cfg.discover_every);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut backoff: BTreeMap<String, (u32, u32)> = BTreeMap::new();
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if st.cfg.discover {
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
                    probe_once(&st, &mut backoff).await;
                }
                // The borrow `wait_for` hands back is not `Send` and this
                // future has to be: discard it inside the block.
                () = async { drop(stop.wait_for(|s| *s).await) } => break,
            }
        }
    });
}

// ----------------------------------------- card 141: a panel that moved ----

/// How long a probe listens for answers.
///
/// Short: a panel that is there answers a `GET_INFO` in milliseconds, and this
/// runs on the browse's tick, where anything longer is time the next browse
/// waits for. It is clamped to the tick so a fast test cannot overlap itself.
const PROBE_WINDOW: Duration = Duration::from_secs(1);

/// The one key the probe's backoff is kept under: the probe is one thing for
/// the whole fleet, not one per device.
const PROBE: &str = "probe";

/// Is the probe on? Card 141: on wherever the browse is, and also wherever
/// somebody has named the addresses to ask - which is a container with no
/// broadcast route, and the tests.
fn probing(st: &AppState) -> bool {
    st.cfg.discover || !st.cfg.probe_to.is_empty()
}

/// Where it asks: what was configured, or the subnet broadcast address of
/// every interface on the machine (spec 5.5).
fn probe_targets(st: &AppState) -> Vec<std::net::SocketAddr> {
    if st.cfg.probe_to.is_empty() {
        screeny::discover::broadcast_targets(screeny::proto::DEFAULT_CONTROL_PORT)
    } else {
        st.cfg.probe_to.clone()
    }
}

/// **Find a panel that has taken a new DHCP lease, without being told.**
///
/// One `GET_INFO` to the broadcast address; every panel answers with its own
/// `id=`; a known id at a new address is that panel, moved
/// ([`devices::Registry::probed`]). The studio's registry is keyed by that id,
/// so following it costs the panel nothing: same player, same patch, same
/// seed, new address.
///
/// Bounded, and each bound is deliberate:
///
/// * **only when something is missing** - no device unheard past
///   `Config::stale_after`, no probe. A house that is working sends nothing.
/// * **one in flight** - this is awaited on the discovery task, which does one
///   thing at a time, and it runs *after* the browse rather than beside it.
/// * **capped, jittered backoff** - [`fail`], the same one the two polls use:
///   a panel that has been off for a month is probed for every twelfth pass,
///   not every pass.
/// * **never on a render thread** - `spawn_blocking`.
/// * **logged per event, not per attempt** - a line when a panel is followed,
///   and a line when the probe itself starts failing for a new reason. A probe
///   that finds nothing says nothing, for ever.
async fn probe_once(st: &AppState, backoff: &mut BTreeMap<String, (u32, u32)>) {
    if !probing(st) {
        return;
    }
    if let Some((_, left)) = backoff.get_mut(PROBE) {
        if *left > 0 {
            *left -= 1;
            return;
        }
    }
    let missing = st.devices.unheard(st.cfg.stale_after);
    if missing.is_empty() {
        // Nothing to look for. Not a failure, and the next panel to go quiet
        // should be looked for at once rather than after a backoff it did not
        // earn.
        backoff.remove(PROBE);
        return;
    }
    let to = probe_targets(st);
    if to.is_empty() {
        return;
    }
    let window = PROBE_WINDOW.min(st.cfg.discover_every);
    let found = tokio::task::spawn_blocking(move || devices::probe(window, &to)).await;
    let (list, err) = match found {
        Ok(Ok(list)) => (list, None),
        Ok(Err(e)) => (Vec::new(), Some(e)),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    let said = st.devices.discovery_health().last_probe_error;
    if let Some(e) = &err {
        if said.as_deref() != Some(e.as_str()) {
            eprintln!("studio: probing for a panel that has moved: {e}");
        }
    }
    let moves = st.devices.probed(&list, &missing, err);
    if moves.is_empty() {
        fail(backoff, PROBE);
        return;
    }
    for m in &moves {
        let from = if m.from.is_empty() { "nowhere we knew of".to_string() } else { m.from.clone() };
        eprintln!("studio: `{}` answered a probe at {} (it was at {from}); following it", m.id, m.to);
    }
    // The player is already keyed by the id, so nothing here touches it: the
    // supervisor re-aims its link at the new address on its next tick.
    backoff.remove(PROBE);
    st.persist();
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
            let asked = tokio::task::spawn_blocking(move || devices::identify_at_counted(addr)).await;
            // Card 164: the GET_INFO is control traffic with this panel
            // whether or not it answered. Recorded against the id it is known
            // by now; adopting a real id below carries the record over.
            let asked = match asked {
                Ok((cost, out)) => {
                    st.devices.metered_control(&id, cost);
                    Ok(out)
                }
                Err(e) => Err(e),
            };
            match asked {
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
        let polled = tokio::task::spawn_blocking(move || {
            devices::control_call(addr, |c| c.telemetry().map_err(|e| e.to_string()))
        })
        .await;
        let polled = match polled {
            Ok((cost, out)) => {
                st.devices.metered_control(&id, cost);
                Ok(out)
            }
            Err(e) => Err(e),
        };
        match polled {
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
/// * *the cap is for firmware that has no server* - card 118: a panel that has
///   answered once climbs the ladder from the bottom like any other failure,
///   and a reboot seen over UDP clears its backoff outright;
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
        // **Card 118: a reboot is the best moment there is to ask.** The
        // telemetry poll saw this panel's uptime go backwards, so everything
        // this poller had decided about it - including a backoff climbed while
        // the panel was refusing connections on its way up - is about the panel
        // before the reboot. This does not poll *faster*: it only forgives the
        // skips, and the tick is still `device_http_every`.
        if st.devices.take_http_retry(&id) {
            backoff.remove(&id);
        }
        if let Some((_, left)) = backoff.get_mut(&id) {
            if *left > 0 {
                *left -= 1;
                continue;
            }
        }
        // Nowhere to ask yet. Not a failure and not worth backing off for:
        // the telemetry poll is what finds out where this device is.
        let Some(addr) = record.http_addr(st.cfg.device_http_port) else { continue };

        let read = tokio::task::spawn_blocking(move || crate::devhttp::get_status_counted(addr, crate::devhttp::TIMEOUT)).await;
        // Card 164: the HTTP path's bytes, recorded before the reply is
        // looked at - a read that failed still cost what it cost.
        let read = match read {
            Ok((cost, out)) => {
                st.devices.metered_http(&id, cost.out, cost.inbound);
                Ok(out)
            }
            Err(e) => Err(e),
        };
        match read {
            Ok(Ok(reply)) => {
                // Said on the first read ever, and again when a device starts
                // answering after it had stopped - which is a panel coming
                // back, or firmware that has grown the API since we last
                // looked. Per *transition*, never per attempt: card 106's
                // rule, and the two lines a day a panel switched off at night
                // produces are each worth reading.
                let announce = record.http.reads == 0 || record.http.last_error.is_some();
                // The reply is handed straight to the registry and never held
                // here: it carries the SSID, and nothing below may print it.
                let fw = reply.fw.to_string();
                // The API's own spelling ("ota_0"), not the enum variant's.
                let slot = serde_json::to_value(reply.fw_slot)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                // Card 199: the one fact this read needs to carry past
                // `heard_http`, which consumes `reply` - whether the panic
                // route is worth a second connection is decided from the
                // `boot_id` alone.
                let boot_id = reply.boot_id;
                if let Some(heard) = st.devices.heard_http(&id, reply) {
                    if announce {
                        eprintln!("studio: `{}` serves its own status API: firmware {fw}, slot {slot}", record.label());
                    }
                    if heard.rebooted {
                        eprintln!("studio: `{}` rebooted: {} since the studio started", record.label(), heard.reboots);
                    }
                    // Card 195, and said plainly rather than alarmingly: on
                    // this firmware a crash and a reflash arrive as the same
                    // `reset_reason`, so the line says what is known and not
                    // what it might mean.
                    if heard.unasked {
                        eprintln!(
                            "studio: `{}` rebooted without the studio asking: {} of {} so far",
                            record.label(),
                            heard.unasked_reboots,
                            heard.reboots
                        );
                    }
                }
                backoff.remove(&id);

                // Card 199: the panic breadcrumb cannot change while the
                // device runs (spec 8.6), so it is asked once per `boot_id`,
                // on this same task, right after the status read whose
                // `boot_id` is new - never on the poll, and never a second
                // connection in flight: this `await` only starts once the one
                // above has finished, so there is still exactly one HTTP
                // connection to this device open at a time.
                if st.devices.want_panic(&id, boot_id) {
                    let panic_read =
                        tokio::task::spawn_blocking(move || crate::devhttp::get_panic_counted(addr, crate::devhttp::TIMEOUT)).await;
                    match panic_read {
                        Ok((cost, out)) => {
                            st.devices.metered_http(&id, cost.out, cost.inbound);
                            st.devices.heard_panic(&id, boot_id, out.ok());
                        }
                        Err(_) => st.devices.heard_panic(&id, boot_id, None),
                    }
                }
            }
            Ok(Err(fault)) => {
                // **Card 118: "absent" is a claim about the firmware** - this
                // panel serves no such API - and a panel that has answered
                // once has already disproved it. A connection refused by a
                // panel whose network stack is up before its HTTP workers are
                // listening is an ordinary failure, and saying otherwise cost
                // the bench two minutes of a stale Device block after every
                // reflash.
                let absent = fault.absent && record.http.reads == 0;
                if st.devices.http_failed(&id, absent, fault.why.clone()) {
                    if absent {
                        eprintln!(
                            "studio: `{}` has no HTTP status API ({fault}); its UDP telemetry is all the studio will show",
                            record.label()
                        );
                    } else {
                        eprintln!("studio: `{}`: reading its status: {fault}", record.label());
                    }
                }
                // A device with no server *and nothing it has ever said to the
                // contrary* is asked at the slowest rate at once rather than
                // climbing to it: a firmware update is the only thing that
                // changes the answer, and that is not a thing that happens
                // twice a minute. One that has answered climbs the ordinary
                // ladder from the bottom instead, so a reboot costs a poll or
                // two rather than the full cap.
                if absent {
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
