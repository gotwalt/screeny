//! The connection to the broker and its lifecycle. Nothing else.
//!
//! Two tasks. The **poller** owns rumqttc's `EventLoop` and does nothing but
//! poll it, for as long as the studio runs: a broker that goes away is waited
//! out with a backoff and polled again, never given up on. The **session**
//! owns the client and decides what to say. They meet at a `watch` (is the
//! link up, how many times has it connected) and a small bounded queue of
//! incoming messages, so neither can hold the other up: the session never
//! waits on the broker - it uses `try_publish`, and while the link is down it
//! says nothing, because every (re)connect republishes everything anyway.
//!
//! The lifecycle, as HA's docs ask for it:
//!
//! - the Last Will is a retained `offline` on the status topic;
//! - **every** ConnAck, first or fiftieth: subscribe to the command topics and
//!   HA's `<prefix>/status` (a clean session keeps no subscriptions), then the
//!   discovery config, `online`, and every state - all retained;
//! - HA saying `online` (it restarted): the discovery config and every state
//!   again;
//! - after that, a state is published only when it changes;
//! - stopping: a retained `offline`, then a clean disconnect.

use super::discovery;
use super::payload;
use super::topics::Topics;
use super::{fleet, Command, Fleet, Ha, MqttConfig, Order, Status, Target};
use rumqttc::{AsyncClient, ConnectionError, Event, EventLoop, LastWill, MqttOptions, Outgoing, Packet, QoS};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

pub const ONLINE: &str = "online";
pub const OFFLINE: &str = "offline";
/// Requests the client may have queued for the poller. A connect publishes
/// about ten; the session never waits for room (`try_publish`).
const REQUESTS: usize = 256;
/// Incoming messages waiting for the session. HA sends a command per click,
/// so this is only ever full if something is flooding the command topics, and
/// then dropping is the right answer.
const INCOMING: usize = 32;
/// Commands waiting for the studio.
pub const COMMANDS: usize = 16;
const KEEP_ALIVE: Duration = Duration::from_secs(30);
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LAST_RETRY: Duration = Duration::from_secs(30);
/// How long stopping may spend saying goodbye before it gives up on it.
pub const GOODBYE: Duration = Duration::from_secs(2);
/// Big enough for a discovery config with every mode a studio may have.
const MAX_PACKET: usize = 64 * 1024;
/// At most one line per this, per kind of complaint, in `docker logs`.
const QUIET_FOR: Duration = Duration::from_secs(60);

/// The running integration. [`Handle::finish`] after the studio's stop signal
/// waits for `offline` to go out.
pub struct Handle {
    task: JoinHandle<()>,
}

impl Handle {
    /// Wait - at most `within` - for the goodbye to be said.
    pub async fn finish(mut self, within: Duration) {
        if tokio::time::timeout(within, &mut self.task).await.is_err() {
            self.task.abort();
        }
    }
}

/// Start talking to the broker. `snapshots` is what to show - every panel's
/// HA device -, `commands` is where HA's requests go, each with its panel, `status` is told how it is going, and `stop` ends
/// it.
#[must_use]
pub fn spawn(
    cfg: MqttConfig,
    snapshots: watch::Receiver<Fleet>,
    commands: mpsc::Sender<Order>,
    status: watch::Sender<Status>,
    stop: watch::Receiver<bool>,
) -> Handle {
    Handle { task: tokio::spawn(run(cfg, snapshots, commands, status, stop)) }
}

/// Card 311: keep the connection [`Ha`] asks for - none, or one - and start
/// it again whenever the settings change it. Runs until the studio stops;
/// the returned handle finishes when the last client has said goodbye.
#[must_use]
pub fn supervise(ha: std::sync::Arc<Ha>, snapshots: watch::Receiver<Fleet>, commands: mpsc::Sender<Order>, mut stop: watch::Receiver<bool>) -> Handle {
    let task = tokio::spawn(async move {
        let mut want = ha.want.subscribe();
        loop {
            let cfg = want.borrow_and_update().clone();
            let (quit_client, client_stop) = watch::channel(false);
            let client = cfg.map(|cfg| {
                eprintln!("studio: home assistant: mqtt://{}:{} as `{}` (discovery prefix `{}`)", cfg.host, cfg.port, cfg.instance, cfg.discovery_prefix);
                ha.status.send_replace(Status::Connecting);
                spawn(cfg, snapshots.clone(), commands.clone(), ha.status.clone(), client_stop)
            });
            let done = tokio::select! {
                changed = want.changed() => changed.is_err(),
                () = async { drop(stop.wait_for(|s| *s).await) } => true,
            };
            let _ = quit_client.send(true);
            if let Some(client) = client {
                client.finish(GOODBYE).await;
            }
            ha.status.send_replace(Status::Off);
            if done {
                return;
            }
        }
    });
    Handle { task }
}

/// `will`: leave the retained `offline` as the Last Will. Everything but
/// [`forget`], which must not leave one behind on a topic it just cleared.
fn options(cfg: &MqttConfig, topics: &Topics, will: bool) -> MqttOptions {
    let mut opts = MqttOptions::new(format!("{}-{}", super::topics::APP, cfg.instance), cfg.host.clone(), cfg.port);
    opts.set_keep_alive(KEEP_ALIVE);
    opts.set_clean_session(true);
    opts.set_max_packet_size(MAX_PACKET, MAX_PACKET);
    if will {
        opts.set_last_will(LastWill::new(&topics.status, OFFLINE, QoS::AtLeastOnce, true));
    }
    if let Some(user) = &cfg.username {
        opts.set_credentials(user.clone(), cfg.password.clone().unwrap_or_default());
    }
    opts
}

/// What the poller tells the session about the link.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Link {
    up: bool,
    /// ConnAcks so far. A change is a (re)connect.
    connects: u64,
}

async fn run(
    cfg: MqttConfig,
    mut snapshots: watch::Receiver<Fleet>,
    commands: mpsc::Sender<Order>,
    status: watch::Sender<Status>,
    mut stop: watch::Receiver<bool>,
) {
    let topics = Topics::new(&cfg);
    let (client, eventloop) = AsyncClient::new(options(&cfg, &topics, true), REQUESTS);
    let (link_tx, mut link) = watch::channel(Link::default());
    let (incoming_tx, mut incoming) = mpsc::channel(INCOMING);
    let closing = Arc::new(AtomicBool::new(false));
    let mut poller = tokio::spawn(poll(eventloop, cfg.clone(), link_tx, incoming_tx, status, Arc::clone(&closing)));
    let mut session = Session::new(cfg, topics, client);
    let mut seen = 0;
    loop {
        tokio::select! {
            changed = link.changed() => {
                if changed.is_err() {
                    break;
                }
                let now = *link.borrow_and_update();
                session.up = now.up;
                if now.up && now.connects != seen {
                    seen = now.connects;
                    session.connected(&snapshots.borrow().clone());
                }
            }
            Some((topic, payload)) = incoming.recv() => {
                let snap = snapshots.borrow().clone();
                session.message(&topic, &payload, &snap, &commands);
            }
            changed = snapshots.changed() => {
                if changed.is_err() {
                    break;
                }
                let snap = snapshots.borrow_and_update().clone();
                session.show(&snap, false);
            }
            () = async { drop(stop.wait_for(|s| *s).await) } => break,
        }
    }
    closing.store(true, Ordering::Relaxed);
    if session.up {
        // Retained, so HA - and anyone who subscribes later - sees the studio
        // went away on purpose. A clean disconnect does not fire the Will.
        let _ = session.client.try_publish(&session.topics.status, QoS::AtLeastOnce, true, OFFLINE);
        let _ = session.client.try_disconnect();
        if tokio::time::timeout(GOODBYE, &mut poller).await.is_err() {
            poller.abort();
        }
    } else {
        poller.abort();
    }
}

/// Poll for ever. Errors are waited out with a backoff, said once rather than
/// once per retry, and never end the loop - only stopping does.
async fn poll(
    mut eventloop: EventLoop,
    cfg: MqttConfig,
    link: watch::Sender<Link>,
    incoming: mpsc::Sender<(String, Vec<u8>)>,
    status: watch::Sender<Status>,
    closing: Arc<AtomicBool>,
) {
    let mut retry = FIRST_RETRY;
    let mut last_error: Option<String> = None;
    let mut dropped = Quiet::default();
    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                retry = FIRST_RETRY;
                if last_error.take().is_some() || link.borrow().connects == 0 {
                    eprintln!("studio: home assistant: connected to mqtt://{}:{}", cfg.host, cfg.port);
                }
                link.send_modify(|l| {
                    l.up = true;
                    l.connects += 1;
                });
                status.send_replace(Status::Connected);
            }
            Ok(Event::Incoming(Packet::Publish(p))) => {
                if incoming.try_send((p.topic, p.payload.to_vec())).is_err() {
                    dropped.say(|n| format!("studio: home assistant: {n} incoming message(s) dropped, the queue is full"));
                }
            }
            Ok(Event::Outgoing(Outgoing::Disconnect)) if closing.load(Ordering::Relaxed) => return,
            Ok(_) => {}
            Err(e) => {
                link.send_if_modified(|l| std::mem::replace(&mut l.up, false));
                if closing.load(Ordering::Relaxed) {
                    return;
                }
                let why = describe(&e);
                status.send_replace(Status::Failed { detail: why.clone() });
                if last_error.as_deref() != Some(why.as_str()) {
                    eprintln!("studio: home assistant: mqtt://{}:{}: {why}; retrying, backing off to {}s", cfg.host, cfg.port, LAST_RETRY.as_secs());
                    last_error = Some(why);
                }
                tokio::time::sleep(retry).await;
                retry = (retry * 2).min(LAST_RETRY);
            }
        }
    }
}

/// A connection error as a sentence, without the parts that change on every
/// retry.
fn describe(e: &ConnectionError) -> String {
    match e {
        ConnectionError::ConnectionRefused(code) => format!("the broker refused the connection ({code:?}); check the username and password"),
        other => other.to_string(),
    }
}

/// One line per [`QUIET_FOR`], saying how many were not said.
#[derive(Default)]
struct Quiet {
    last: Option<Instant>,
    held: u64,
}

impl Quiet {
    fn say(&mut self, line: impl FnOnce(u64) -> String) {
        self.held += 1;
        if self.last.is_none_or(|t| t.elapsed() >= QUIET_FOR) {
            eprintln!("{}", line(self.held));
            self.last = Some(Instant::now());
            self.held = 0;
        }
    }
}

/// The session's side: what has been said, and what to say next.
struct Session {
    cfg: MqttConfig,
    topics: Topics,
    client: AsyncClient,
    up: bool,
    /// The last payload that went out on each retained topic, so a state is
    /// sent when it changes and not on every snapshot.
    said: HashMap<String, String>,
    /// Card 352: the devices announced now - panels, and since card 355
    /// channels - by device id (`screeny_<instance>[_<key>]`), so one that has
    /// gone can be cleared.
    published: Vec<Topics>,
    /// What a bare "on" restores, per panel (by the same id).
    last_lit: HashMap<String, Option<u8>>,
    refused: Quiet,
    full: Quiet,
    /// Card 368: the stale device ids already cleared on this connection, so
    /// the clear is said once per role change and not every second.
    cleared: Vec<String>,
}

impl Session {
    fn new(cfg: MqttConfig, topics: Topics, client: AsyncClient) -> Session {
        Session {
            cfg,
            topics,
            client,
            up: false,
            said: HashMap::new(),
            published: Vec::new(),
            last_lit: HashMap::new(),
            refused: Quiet::default(),
            full: Quiet::default(),
            cleared: Vec::new(),
        }
    }

    /// Card 368: the retained topics of an identity a panel no longer has
    /// ([`fleet::stale_topics`]) that have not been cleared since the
    /// connection came up. Idempotent: the same panel in the same role asks
    /// nothing twice; a role change (a panel added, forgotten, a different
    /// first) asks again.
    fn stale(&mut self, fleet: &Fleet) -> Vec<String> {
        let topics = fleet::stale_topics(&self.cfg, fleet);
        let id = topics.first().cloned().unwrap_or_default();
        if topics.is_empty() {
            self.cleared.clear();
            return topics;
        }
        if self.cleared.contains(&id) {
            return Vec::new();
        }
        self.cleared = vec![id];
        topics
    }

    /// A ConnAck: subscribe again, then say everything.
    fn connected(&mut self, fleet: &Fleet) {
        let mut topics: Vec<String> = self.topics.command_wildcards().to_vec();
        topics.push(self.topics.ha_status.clone());
        for topic in topics {
            if let Err(e) = self.client.try_subscribe(&topic, QoS::AtLeastOnce) {
                eprintln!("studio: home assistant: subscribing to {topic}: {e}");
            }
        }
        // Card 310: an empty retained payload deletes a retained message, so
        // the timetable's old states do not outlive it on the broker. Card
        // 355 the same for a later panel's picture and patch, which are its
        // channel's now.
        for device in fleet::devices(&self.cfg, fleet) {
            for topic in device.retired_state_topics() {
                let _ = self.client.try_publish(topic, QoS::AtLeastOnce, true, Vec::new());
            }
        }
        self.cleared.clear();
        self.announce(fleet);
    }

    /// The discovery configs, `online`, and every state, whatever was said
    /// before.
    fn announce(&mut self, fleet: &Fleet) {
        self.said.clear();
        self.show(fleet, true);
    }

    /// Publish one retained message, remembering it.
    fn say(&mut self, topic: String, payload: String) {
        match self.client.try_publish(&topic, QoS::AtLeastOnce, true, payload.clone()) {
            Ok(()) => {
                self.said.insert(topic, payload);
            }
            Err(e) => self.full.say(|n| format!("studio: home assistant: {n} message(s) not sent: {e}")),
        }
    }

    /// Everything that differs from what was last said - or everything, with
    /// `all`. Only while the link is up: a reconnect says it all anyway.
    ///
    /// A panel or channel that has gone since the last time - forgotten,
    /// deleted, or renamed to a new id - has its device removed: an empty
    /// retained config, and every state cleared.
    fn show(&mut self, fleet: &Fleet, all: bool) {
        for view in &fleet.panels {
            let id = fleet::topics_of(&self.cfg, view).device_id;
            let lit = self.last_lit.get(&id).copied().flatten();
            self.last_lit.insert(id, payload::lit(lit, &view.snapshot));
        }
        if !self.up {
            return;
        }
        let now = fleet::devices(&self.cfg, fleet);
        let before = std::mem::take(&mut self.published);
        // A device that is new to this session: what card 352 (a later
        // panel's picture and patch) or card 310 left retained for it goes.
        for new in now.iter().filter(|t| !before.iter().any(|b| b.device_id == t.device_id)) {
            for topic in new.retired_state_topics() {
                let _ = self.client.try_publish(topic, QoS::AtLeastOnce, true, Vec::new());
            }
        }
        for gone in before {
            if now.iter().any(|t| t.device_id == gone.device_id) {
                continue;
            }
            for topic in fleet::retained_topics(&gone) {
                self.said.remove(&topic);
                let _ = self.client.try_publish(&topic, QoS::AtLeastOnce, true, Vec::new());
            }
            self.last_lit.remove(&gone.device_id);
        }
        self.published = now;
        for topic in self.stale(fleet) {
            self.said.remove(&topic);
            let _ = self.client.try_publish(topic, QoS::AtLeastOnce, true, Vec::new());
        }
        // The config before anything that refers to it, and `online` before
        // the states, so HA never sees an entity's state without the entity.
        for m in fleet::announce(&self.cfg, fleet) {
            if !all && self.said.get(&m.topic) == Some(&m.payload) {
                continue;
            }
            self.say(m.topic, m.payload);
        }
    }

    /// Something arrived on a topic this subscribed to.
    fn message(&mut self, topic: &str, payload: &[u8], fleet: &Fleet, commands: &mpsc::Sender<Order>) {
        if topic == self.topics.ha_status {
            if payload == ONLINE.as_bytes() {
                self.announce(fleet);
            }
            return;
        }
        // Whose topic is it? Only a panel or channel this studio has owns
        // one; a command for one that has gone is stale and dropped.
        let panel = fleet.panels.iter().find(|v| fleet::topics_of(&self.cfg, v).command_topics().contains(&topic));
        let channel = || fleet::channel_devices(fleet).find(|c| Topics::for_channel(&self.cfg, c.id).command_topics().contains(&topic));
        let mut channel_id = None;
        let (topics, snapshot, last_lit) = if let Some(view) = panel {
            let topics = fleet::topics_of(&self.cfg, view);
            let last_lit = self.last_lit.get(&topics.device_id).copied().flatten();
            (topics, &view.snapshot, last_lit)
        } else if let Some(ch) = channel() {
            channel_id = Some(ch.id);
            (Topics::for_channel(&self.cfg, ch.id), &ch.snapshot, None)
        } else {
            return;
        };
        match payload::parse_command(&topics, topic, payload, snapshot, last_lit) {
            Ok(command) => {
                // What it is for. A picture is a channel's: the first panel's
                // select is Channel 1's (card 355), a channel device's its own.
                let target = match (&command, panel) {
                    (Command::ShowPicture { .. }, Some(_)) => Target::Channel(crate::state::HOME_CHANNEL),
                    (Command::ShowPicture { .. }, None) => Target::Channel(channel_id.unwrap_or(crate::state::HOME_CHANNEL)),
                    (_, Some(view)) => Target::Panel(view.device.clone()),
                    (_, None) => return,
                };
                if commands.try_send(Order { target, command }).is_err() {
                    self.full.say(|n| format!("studio: home assistant: {n} command(s) dropped, the studio is behind"));
                }
            }
            Err(why) => self.refused.say(|n| {
                if n > 1 {
                    format!("studio: home assistant: refused {why} (and {} more since the last line)", n - 1)
                } else {
                    format!("studio: home assistant: refused {why}")
                }
            }),
        }
    }
}

/// Take this studio out of HA: connect, publish an empty retained payload on
/// the discovery topic and on every retained topic of ours, and go. HA drops
/// the entities and the device. The Settings screen's "Remove from Home
/// Assistant", which switches the integration off first - this connects as
/// the same client, and two of those would take turns throwing each other off.
///
/// `panels` are the keys of every panel's HA device now (`None` is the
/// first): each one's config and states are cleared; `channels` are the ids of
/// every channel that has a device (card 355). A panel or channel gone
/// earlier cleared its own when it went.
///
/// # Errors
///
/// The broker could not be reached, or refused us, within `within`.
pub async fn forget(cfg: &MqttConfig, panels: &[Option<String>], channels: &[u32], within: Duration) -> Result<(), String> {
    let topics = Topics::new(cfg);
    let (client, mut eventloop) = AsyncClient::new(options(cfg, &topics, false), REQUESTS);
    let work = async {
        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::ConnAck(_))) => break,
                Ok(_) => {}
                Err(e) => return Err(describe(&e)),
            }
        }
        let empty = discovery::remove_device(&topics).1;
        let mut clear = vec![topics.status.clone()];
        // The first panel always, whatever `panels` says.
        let mut keys: Vec<Option<String>> = vec![None];
        keys.extend(panels.iter().filter(|k| k.is_some()).cloned());
        for key in keys {
            clear.extend(fleet::retained_topics(&Topics::for_panel(cfg, key.as_deref())));
        }
        for id in channels {
            clear.extend(fleet::retained_topics(&Topics::for_channel(cfg, *id)));
        }
        for topic in clear {
            client.publish(topic, QoS::AtLeastOnce, true, empty.clone()).await.map_err(|e| e.to_string())?;
        }
        client.disconnect().await.map_err(|e| e.to_string())?;
        loop {
            match eventloop.poll().await {
                Ok(Event::Outgoing(Outgoing::Disconnect)) | Err(_) => return Ok(()),
                Ok(_) => {}
            }
        }
    };
    tokio::time::timeout(within, work).await.map_err(|_| format!("mqtt://{}:{}: no answer within {}s", cfg.host, cfg.port, within.as_secs()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ha::payload::tests::config;
    use crate::ha::{ChannelView, PanelView, Snapshot};

    fn view(device: &str, key: Option<&str>) -> PanelView {
        PanelView { device: device.into(), key: key.map(str::to_string), name: device.into(), snapshot: Snapshot::default() }
    }

    fn session() -> Session {
        let cfg = config();
        let topics = Topics::new(&cfg);
        let (client, _eventloop) = AsyncClient::new(options(&cfg, &topics, false), REQUESTS);
        Session::new(cfg, topics, client)
    }

    fn fleet(panels: Vec<PanelView>) -> Fleet {
        Fleet { panels, channels: vec![ChannelView { id: 1, name: "Channel 1".into(), snapshot: Snapshot::default() }] }
    }

    /// Card 368: the first panel clears the identity it had as a later panel,
    /// once; a different first clears its own; a reconnect says it again.
    #[test]
    fn the_first_panel_clears_the_device_it_was_as_a_later_panel_once() {
        let mut s = session();
        let office_first = fleet(vec![view("office-1", None), view("living-2", Some("living-2"))]);
        let cleared = s.stale(&office_first);
        assert_eq!(cleared[0], "homeassistant/device/screeny_studio_office-1/config");
        assert!(cleared.contains(&"screeny/studio/office-1/firmware/state".to_string()));
        assert!(!cleared.iter().any(|t| t.contains("living-2")), "a live device is never touched");
        assert!(s.stale(&office_first).is_empty(), "idempotent: said once");
        // Living Room becomes first: Office is a later panel with a live
        // identity, and Living Room's old one is cleared.
        let living_first = fleet(vec![view("living-2", None), view("office-1", Some("office-1"))]);
        let cleared = s.stale(&living_first);
        assert_eq!(cleared[0], "homeassistant/device/screeny_studio_living-2/config");
        assert!(s.stale(&living_first).is_empty());
        // A reconnect forgets what was said.
        s.cleared.clear();
        assert_eq!(s.stale(&living_first)[0], "homeassistant/device/screeny_studio_living-2/config");
        // No panel: nothing to clear.
        assert!(s.stale(&fleet(vec![view("", None)])).is_empty());
    }
}
