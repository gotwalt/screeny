//! The one place that knows both the studio and this module: it reads a
//! [`Snapshot`] out of the studio and carries a [`Command`] back in, through
//! the same functions the page's routes use.
//!
//! **Card 350 left this on the first panel.** The studio has several panels
//! now; Home Assistant still sees one device, and it is the first panel - the
//! one a route without `panel` means - with the same topics, discovery id and
//! `unique_id`s as before. Card 352 gives every panel an HA device of its own.

use super::client::{self, Handle};
use super::{Command, PatchState, Picture, Snapshot};
use crate::state::DEFAULT_SETTING;
use crate::AppState;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, watch};

/// How often the snapshot is taken even when no state event said to. Some
/// of what HA is shown - the panel link, a brightness the device capped - moves
/// without one.
const LOOK_EVERY: Duration = Duration::from_secs(1);

/// Start the integration against this studio: the snapshot and the command
/// tasks, which cost nothing while there is no broker, and the supervisor that
/// connects - or does not - as [`crate::ha::Ha`]'s settings say, from now on.
#[must_use]
pub fn start(st: &AppState) -> Handle {
    let (snap_tx, snap_rx) = watch::channel(snapshot(st));
    let (cmd_tx, cmd_rx) = mpsc::channel(client::COMMANDS);
    tokio::spawn(watch_studio(st.clone(), snap_tx));
    tokio::spawn(obey(st.clone(), cmd_rx));
    client::supervise(std::sync::Arc::clone(&st.ha), snap_rx, cmd_tx, st.stop.clone())
}

/// What HA should be shown now.
#[must_use]
pub fn snapshot(st: &AppState) -> Snapshot {
    let first = st.first();
    let page = st.state_of(&first);
    let status = st.panels.status_of(&first);
    let pictures = pictures(st);
    // The working copy changed since its setting was loaded is not a picture
    // in the list: HA shows it as unknown rather than naming something the
    // panel is not showing.
    let picture = (!page.modified).then(|| Picture::label(&status.patch_name, &page.setting));
    Snapshot {
        patch: PatchState {
            id: page.patch.clone(),
            name: status.patch_name.clone(),
            setting: page.setting.clone(),
            modified: page.modified,
        },
        brightness: status.brightness,
        picture: picture.filter(|l| pictures.iter().any(|p| &p.label == l)),
        pictures,
        panel_connected: status.on && status.panel.as_ref().is_some_and(|p| p.connected),
    }
}

/// Every patch this studio can play here, on Default and then on each of its
/// named settings (alphabetically, as the Picture screen lists them).
#[must_use]
pub fn pictures(st: &AppState) -> Vec<Picture> {
    let gpu = screeny_art::gpu_status().available;
    let mut out = Vec::new();
    for p in crate::page::patches(st.cfg.fault_patches) {
        if p.needs_gpu && !gpu {
            continue;
        }
        let settings = std::iter::once(DEFAULT_SETTING.to_string()).chain(st.memory.setting_names(p.id));
        out.extend(settings.map(|setting| Picture { label: Picture::label(p.name, &setting), patch: p.id.to_string(), setting }));
    }
    out
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

/// The nearest real brightness stop to `level` (card 187), so the level HA
/// is shown back is the one the panel really has. A tie goes to the dimmer
/// stop.
///
/// **Zero is dark and nothing else is.** A nonzero level snaps to the nearest
/// *nonzero* stop, as the firmware raises anything dim-but-nonzero to its
/// floor: HA's 1% is "the dimmest the panel can show", never off.
#[must_use]
pub fn snap(level: u8) -> u8 {
    if level == 0 {
        return 0;
    }
    crate::page::brightness_stops()
        .into_iter()
        .filter(|s| *s > 0)
        .min_by_key(|s| (i16::from(*s) - i16::from(level)).unsigned_abs())
        .unwrap_or(level)
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
            let level = snap(*level);
            st.first().set_brightness(Some(level));
        }
        Command::ShowPicture { patch, setting } => {
            if !st.memory.setting_names(patch).iter().any(|n| n == setting) && !crate::state::is_default_name(setting) {
                return Err(format!("`{patch}` has no setting called `{setting}` any more"));
            }
            // A hand change, like picking a patch and a setting on the page:
            // card 350's three rules, and the manual cross-fade (card 304).
            let def = crate::channel::find_patch(patch, st.cfg.fault_patches).ok_or_else(|| format!("no patch called `{patch}`"))?;
            let panel = st.first();
            let channel = st.panels.pick(&panel, def, setting, false, None)?;
            channel.ensure_running();
            crate::fleet::aim_at_device(st, &panel);
        }
    }
    st.changed(None);
    st.persist();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::snap;

    #[test]
    fn snap_lands_on_a_real_stop_and_never_turns_a_level_off() {
        let stops = crate::page::brightness_stops();
        assert_eq!(snap(0), 0);
        assert_eq!(snap(1), stops[1], "1 is the dimmest visible stop, not dark");
        assert_eq!(snap(255), *stops.last().unwrap());
        for level in 0..=255u8 {
            assert!(stops.contains(&snap(level)), "{level} -> {}", snap(level));
        }
    }
}
