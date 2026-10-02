//! The one place that knows both the studio and this module: it reads a
//! [`Snapshot`] out of the studio and carries a [`Command`] back in, through
//! the same functions the page's routes use.
//!
//! **One HA device per panel (card 352) and per channel (card 355).** The
//! first panel - the one a route without `panel` means - is the device every
//! earlier build announced, with the same topics, discovery id and
//! `unique_id`s, and its picture is **Channel 1's**; every later panel is a
//! device of its own, named after the panel, with a channel select; every
//! other channel is a device of its own with the picture. A panel or channel
//! that appears while connected gets its discovery published, and one that
//! goes has its removed, on the next look (a state event, or the
//! once-a-second tick): the studio needs no notification hooks for that. The
//! commands go through the routes' own functions ([`crate::fleet::move_to_channel`],
//! [`crate::panels::Panels::pick`]).

use super::client::{self, Handle};
use super::{fleet, ChannelOption, ChannelView, Command, Fleet, Order, PanelView, PatchState, Picture, Snapshot, Target};
use crate::state::DEFAULT_SETTING;
use crate::channel::Channel;
use crate::panel::Panel;
use crate::AppState;
use std::sync::Arc;
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
    let (snap_tx, snap_rx) = watch::channel(fleet_of(st));
    let (cmd_tx, cmd_rx) = mpsc::channel(client::COMMANDS);
    tokio::spawn(watch_studio(st.clone(), snap_tx));
    tokio::spawn(obey(st.clone(), cmd_rx));
    client::supervise(std::sync::Arc::clone(&st.ha), snap_rx, cmd_tx, st.stop.clone())
}

/// What HA should be shown now: every panel's device, first panel first, and
/// every channel.
#[must_use]
pub fn fleet_of(st: &AppState) -> Fleet {
    let panels = st.panels.all();
    let pictures = pictures(st);
    let channels = st.panels.channels();
    let options = ChannelOption::all(&channels.iter().map(|c| (c.id(), c.name())).collect::<Vec<_>>());
    // Channel 1's picture is the first device's (card 355).
    let home = picture_of(st, &st.panels.home(), &pictures);
    let views = if panels.is_empty() {
        // With no panel at all, the one device HA is shown is Channel 1's
        // picture, with no light behind it.
        vec![PanelView { device: String::new(), key: None, name: String::new(), snapshot: Snapshot { channels: options.clone(), ..home } }]
    } else {
        let devices: Vec<String> = panels.iter().map(|p| p.device()).collect();
        panels
            .iter()
            .zip(fleet::keys(&devices))
            .map(|(panel, key)| {
                let mut snapshot = Snapshot { channels: options.clone(), channel: panel.channel().map(|c| c.id()), ..panel_of(st, panel) };
                if key.is_none() {
                    snapshot = Snapshot { patch: home.patch.clone(), pictures: home.pictures.clone(), picture: home.picture.clone(), ..snapshot };
                }
                PanelView {
                    device: panel.device(),
                    name: match &key {
                        // The first panel's device is named on the Settings screen.
                        None => String::new(),
                        Some(_) => st.devices.get(&panel.device()).map_or_else(|| panel.device(), |d| d.label()),
                    },
                    key,
                    snapshot,
                }
            })
            .collect()
    };
    let channels = channels.iter().map(|c| ChannelView { id: c.id(), name: c.name(), snapshot: picture_of(st, c, &pictures) }).collect();
    Fleet { panels: views, channels }
}

/// The keys of every panel's HA device now, first (`None`) first: what
/// "Remove from Home Assistant" clears.
#[must_use]
pub fn panel_keys(st: &AppState) -> Vec<Option<String>> {
    let devices: Vec<String> = st.panels.all().iter().map(|p| p.device()).collect();
    if devices.is_empty() {
        return vec![None];
    }
    fleet::keys(&devices)
}

/// The ids of the channels that have an HA device of their own: all but
/// Channel 1, which the first panel's is.
#[must_use]
pub fn channel_ids(st: &AppState) -> Vec<u32> {
    st.panels.channels().iter().map(|c| c.id()).filter(|id| *id != crate::state::HOME_CHANNEL).collect()
}

/// What HA should be shown of the first panel: the single-device studio's
/// snapshot.
#[must_use]
pub fn snapshot(st: &AppState) -> Snapshot {
    fleet_of(st).panels.swap_remove(0).snapshot
}

/// A channel's picture, as HA sees it: the patch on its setting, the list of
/// pictures to pick from and the one playing.
fn picture_of(st: &AppState, channel: &Arc<Channel>, pictures: &[Picture]) -> Snapshot {
    let page = st.state_of(channel, None);
    let patch_name = crate::channel::find_patch(&page.patch, st.cfg.fault_patches).map_or("", |d| d.name).to_string();
    // The working copy changed since its setting was loaded is not a picture
    // in the list: HA shows it as unknown rather than naming something the
    // channel is not showing.
    let picture = (!page.modified).then(|| Picture::label(&patch_name, &page.setting));
    Snapshot {
        patch: PatchState { id: page.patch.clone(), name: patch_name, setting: page.setting.clone(), modified: page.modified },
        picture: picture.filter(|l| pictures.iter().any(|p| &p.label == l)),
        pictures: pictures.to_vec(),
        ..Snapshot::default()
    }
}

/// A panel's own part of its device: brightness, and the link.
fn panel_of(st: &AppState, panel: &Arc<Panel>) -> Snapshot {
    let status = st.panels.status_of(panel);
    Snapshot {
        brightness: status.brightness,
        panel_connected: status.on && status.panel.as_ref().is_some_and(|p| p.connected),
        ..Snapshot::default()
    }
}

/// The patches this machine can play, by the same predicate the page reads
/// (card 357): HA's list leaves the others out.
#[must_use]
pub fn playable_patches(faults: bool, gpu: &screeny_art::GpuStatus) -> Vec<crate::page::PatchInfo> {
    crate::page::patches(faults, gpu).into_iter().filter(|p| p.playable).collect()
}

/// Every patch this studio can play here, on Default and then on each of its
/// named settings (alphabetically, as the Picture screen lists them).
#[must_use]
pub fn pictures(st: &AppState) -> Vec<Picture> {
    let mut out = Vec::new();
    for p in playable_patches(st.cfg.fault_patches, &screeny_art::gpu_status()) {
        let settings = std::iter::once(DEFAULT_SETTING.to_string()).chain(st.memory.setting_names(p.id));
        out.extend(settings.map(|setting| Picture { label: Picture::label(p.name, &setting), patch: p.id.to_string(), setting }));
    }
    out
}

/// Take a snapshot on every state event and once a second, and pass it on
/// when it differs from the last.
async fn watch_studio(st: AppState, out: watch::Sender<Fleet>) {
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
        let now = fleet_of(&st);
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
async fn obey(st: AppState, mut orders: mpsc::Receiver<Order>) {
    while let Some(order) = orders.recv().await {
        if let Err(why) = execute(&st, &order) {
            eprintln!("studio: home assistant: {order:?}: {why}");
        }
    }
}

/// One command for one panel or channel, through the same paths the page and
/// the API take ([`crate::fleet::move_to_channel`] for `POST /panel/channel`,
/// [`crate::panels::Panels::pick`] for a picture). Every change reaches the
/// browsers and the state file, like any other.
///
/// # Errors
///
/// What the studio said no with, in a sentence.
pub fn execute(st: &AppState, order: &Order) -> Result<(), String> {
    match (&order.target, &order.command) {
        // A picture is a channel's: every panel on it changes, with the fade.
        (Target::Channel(id), Command::ShowPicture { patch, setting }) => {
            let channel = st.panels.channel(*id).ok_or_else(|| format!("no channel {id} any more"))?;
            if !st.memory.setting_names(patch).iter().any(|n| n == setting) && !crate::state::is_default_name(setting) {
                return Err(format!("`{patch}` has no setting called `{setting}` any more"));
            }
            let def = crate::channel::find_patch(patch, st.cfg.fault_patches).ok_or_else(|| format!("no patch called `{patch}`"))?;
            st.panels.pick(&channel, def, setting, None)?;
            channel.ensure_running();
        }
        (Target::Channel(_), other) => return Err(format!("{other:?} is not something a channel does")),
        (Target::Panel(device), command) => {
            // An empty id is the first panel, or - with none - nothing.
            let panel = if device.is_empty() { st.first() } else { Some(st.panel(Some(device))?) };
            let panel = panel.ok_or_else(|| "there is no panel yet".to_string())?;
            match command {
                Command::SetBrightness(level) => {
                    // The nearest real stop, so what HA is shown back is what
                    // the panel does. The supervisor sends it within a second.
                    panel.set_brightness(Some(snap(*level)));
                }
                Command::MoveToChannel(id) => {
                    let channel = st.panels.channel(*id).ok_or_else(|| format!("no channel {id} any more"))?;
                    crate::fleet::move_to_channel(st, &panel, &channel);
                }
                Command::ShowPicture { .. } => return Err("a picture is a channel's, not a panel's".into()),
            }
        }
    }
    st.changed(None);
    st.persist();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{playable_patches, snap};

    fn status(available: bool, software: bool) -> screeny_art::GpuStatus {
        screeny_art::GpuStatus {
            available,
            adapter: if available { "llvmpipe (LLVM 19.1.7, 128 bits)".into() } else { String::new() },
            backend: if available { "Vulkan".into() } else { String::new() },
            software,
            error: (!available).then(|| "no GPU adapter: none".into()),
        }
    }

    /// Card 357: on a software rasteriser HA is not offered overland or ghosts
    /// but still gets leaves, lattice and knot; with a hardware adapter, all.
    #[test]
    #[cfg(feature = "gpu")]
    fn ha_leaves_out_the_hardware_only_patches_on_a_software_adapter() {
        let ids = |g: &screeny_art::GpuStatus| playable_patches(false, g).iter().map(|p| p.id).collect::<Vec<_>>();
        let hard = ids(&status(true, false));
        let soft = ids(&status(true, true));
        let none = ids(&status(false, false));
        assert!(hard.contains(&"overland") && hard.contains(&"ghosts"));
        assert!(!soft.contains(&"overland") && !soft.contains(&"ghosts"));
        for id in ["leaves", "lattice", "knot", "vesta"] {
            assert!(soft.contains(&id), "{id} stays on software");
        }
        for id in ["leaves", "lattice", "knot", "overland", "ghosts"] {
            assert!(!none.contains(&id), "{id} needs an adapter");
        }
        assert!(none.contains(&"vesta"));
    }

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
