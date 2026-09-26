//! The one place that knows both the studio and this module: it reads a
//! [`Snapshot`] out of the studio and carries a [`Command`] back in, through
//! the same functions the page's routes use.

use super::client::{self, Handle};
use super::{Command, MqttConfig, PatchState, ScheduleState, Snapshot};
use crate::player::PlayerChange;
use crate::{schedule, AppState};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, watch};

/// How often the snapshot is taken even when no state event said to. Some
/// of what HA is shown - the panel link, a brightness the device capped - moves
/// without one.
const LOOK_EVERY: Duration = Duration::from_secs(1);

/// Start the integration against this studio.
#[must_use]
pub fn start(st: &AppState, cfg: MqttConfig) -> Handle {
    eprintln!(
        "studio: home assistant: mqtt://{}:{} as `{}` (discovery prefix `{}`)",
        cfg.host, cfg.port, cfg.instance, cfg.discovery_prefix
    );
    let (snap_tx, snap_rx) = watch::channel(snapshot(st));
    let (cmd_tx, cmd_rx) = mpsc::channel(client::COMMANDS);
    tokio::spawn(watch_studio(st.clone(), snap_tx));
    tokio::spawn(obey(st.clone(), cmd_rx));
    client::spawn(cfg, snap_rx, cmd_tx, st.stop.clone())
}

/// What HA should be shown now.
#[must_use]
pub fn snapshot(st: &AppState) -> Snapshot {
    let page = st.page_state();
    let status = st.page().status();
    Snapshot {
        patch: PatchState {
            id: page.studio.patch.clone(),
            name: status.patch_name.clone(),
            setting: page.studio.setting.clone(),
            modified: page.studio.modified,
        },
        brightness: status.brightness,
        scenes: page.modes.iter().map(|m| m.name.clone()).collect(),
        scene: schedule::matching_mode(st),
        schedule: ScheduleState {
            enabled: page.schedule.enabled,
            due: page.mode.clone(),
            until: page.until.clone(),
            overridden: page.overridden,
            note: page.schedule_note.clone(),
        },
        panel_connected: status.on && status.panel.as_ref().is_some_and(|p| p.connected),
    }
}

/// Take a snapshot on every state event and once a second, and pass it on
/// when it differs from the last.
async fn watch_studio(st: AppState, out: watch::Sender<Snapshot>) {
    let mut states = st.states.subscribe();
    let mut stop = st.stop.clone();
    let mut ticker = tokio::time::interval(LOOK_EVERY);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            got = states.recv() => {
                // Falling behind is fine: the snapshot is taken fresh.
                if matches!(got, Err(broadcast::error::RecvError::Closed)) {
                    break;
                }
            }
            _ = ticker.tick() => {}
            () = async { drop(stop.wait_for(|s| *s).await) } => break,
        }
        let now = snapshot(&st);
        out.send_if_modified(|was| {
            if *was == now {
                return false;
            }
            *was = now;
            true
        });
    }
}

/// Carry out HA's commands, one at a time, until the client goes.
async fn obey(st: AppState, mut commands: mpsc::Receiver<Command>) {
    while let Some(cmd) = commands.recv().await {
        if let Err(why) = execute(&st, &cmd) {
            eprintln!("studio: home assistant: {cmd:?}: {why}");
        }
    }
}

/// One command, through the same paths the page takes. Every change reaches
/// the browsers and the state file, like any other.
///
/// # Errors
///
/// What the studio said no with, in a sentence.
pub fn execute(st: &AppState, cmd: &Command) -> Result<(), String> {
    match cmd {
        Command::SetBrightness(level) => {
            // The nearest real stop, so what HA is shown back is what the
            // panel does. The supervisor sends it within a second.
            let level = schedule::snap_brightness(*level);
            st.page().configure(&PlayerChange { brightness: Some(Some(level)), ..PlayerChange::default() })?;
        }
        Command::ApplyScene(name) => {
            // A hand change, like `/mode/apply`: the manual fade, and the
            // schedule holds until its next entry.
            let note = schedule::apply(st, name, false)?;
            st.book.set_note(note);
        }
        Command::SetSchedule(on) => {
            {
                let mut plan = st.book.plan();
                let entries = plan.schedule.entries.clone();
                plan.set_schedule(*on, &entries)?;
            }
            // As `/schedule/set`: switching it on applies what is due now.
            if schedule::tick(st) {
                return Ok(());
            }
        }
        // Publishes and persists on its own.
        Command::ResumeSchedule => return schedule::resume(st),
    }
    st.publish_state(None, st.page_state());
    st.persist();
    Ok(())
}
