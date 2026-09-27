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
use super::payload::{self, Message};
use super::topics::Topics;
use super::{Command, MqttConfig, Snapshot};
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
const REQUESTS: usize = 64;
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

/// Start talking to the broker. `snapshots` is what to show, `commands` is
/// where HA's requests go, and `stop` ends it.
#[must_use]
pub fn spawn(cfg: MqttConfig, snapshots: watch::Receiver<Snapshot>, commands: mpsc::Sender<Command>, stop: watch::Receiver<bool>) -> Handle {
    Handle { task: tokio::spawn(run(cfg, snapshots, commands, stop)) }
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

async fn run(cfg: MqttConfig, mut snapshots: watch::Receiver<Snapshot>, commands: mpsc::Sender<Command>, mut stop: watch::Receiver<bool>) {
    let topics = Topics::new(&cfg);
    let (client, eventloop) = AsyncClient::new(options(&cfg, &topics, true), REQUESTS);
    let (link_tx, mut link) = watch::channel(Link::default());
    let (incoming_tx, mut incoming) = mpsc::channel(INCOMING);
    let closing = Arc::new(AtomicBool::new(false));
    let mut poller = tokio::spawn(poll(eventloop, cfg.clone(), link_tx, incoming_tx, Arc::clone(&closing)));
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
        ConnectionError::ConnectionRefused(code) => format!("the broker refused the connection ({code:?}); check SCREENY_MQTT_USER and the password"),
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
    /// What a bare "on" restores.
    last_lit: Option<u8>,
    refused: Quiet,
    full: Quiet,
}

impl Session {
    fn new(cfg: MqttConfig, topics: Topics, client: AsyncClient) -> Session {
        Session { cfg, topics, client, up: false, said: HashMap::new(), last_lit: None, refused: Quiet::default(), full: Quiet::default() }
    }

    /// A ConnAck: subscribe again, then say everything.
    fn connected(&mut self, snap: &Snapshot) {
        let mut topics: Vec<&str> = self.topics.command_topics().to_vec();
        topics.push(&self.topics.ha_status);
        for topic in topics {
            if let Err(e) = self.client.try_subscribe(topic, QoS::AtLeastOnce) {
                eprintln!("studio: home assistant: subscribing to {topic}: {e}");
            }
        }
        // Card 310: an empty retained payload deletes a retained message, so
        // the timetable's old states do not outlive it on the broker.
        for topic in self.topics.retired_state_topics() {
            let _ = self.client.try_publish(topic, QoS::AtLeastOnce, true, Vec::new());
        }
        self.announce(snap);
    }

    /// The discovery config, `online`, and every state, whatever was said
    /// before.
    fn announce(&mut self, snap: &Snapshot) {
        self.said.clear();
        self.show(snap, true);
    }

    /// Everything that differs from what was last said - or everything, with
    /// `all`. Only while the link is up: a reconnect says it all anyway.
    fn show(&mut self, snap: &Snapshot, all: bool) {
        self.last_lit = payload::lit(self.last_lit, snap);
        if !self.up {
            return;
        }
        let config = discovery::payload(&self.cfg, &self.topics, snap).to_string();
        // The config before anything that refers to it, and `online` before
        // the states, so HA never sees an entity's state without the entity.
        let mut out = vec![
            Message { topic: self.topics.discovery.clone(), payload: config },
            Message { topic: self.topics.status.clone(), payload: ONLINE.to_string() },
        ];
        out.extend(payload::state_messages(&self.topics, snap));
        for m in out {
            if !all && self.said.get(&m.topic) == Some(&m.payload) {
                continue;
            }
            match self.client.try_publish(&m.topic, QoS::AtLeastOnce, true, m.payload.clone()) {
                Ok(()) => {
                    self.said.insert(m.topic, m.payload);
                }
                Err(e) => self.full.say(|n| format!("studio: home assistant: {n} message(s) not sent: {e}")),
            }
        }
    }

    /// Something arrived on a topic this subscribed to.
    fn message(&mut self, topic: &str, payload: &[u8], snap: &Snapshot, commands: &mpsc::Sender<Command>) {
        if topic == self.topics.ha_status {
            if payload == ONLINE.as_bytes() {
                self.announce(snap);
            }
            return;
        }
        match payload::parse_command(&self.topics, topic, payload, snap, self.last_lit) {
            Ok(cmd) => {
                if commands.try_send(cmd).is_err() {
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
/// the entities and the device. For decommissioning, or before changing
/// `SCREENY_MQTT_ID`.
///
/// # Errors
///
/// The broker could not be reached, or refused us, within `within`.
pub async fn forget(cfg: &MqttConfig, within: Duration) -> Result<(), String> {
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
        let (discovery_topic, empty) = discovery::remove_device(&topics);
        let mut clear = vec![discovery_topic, topics.status.clone()];
        clear.extend(payload::state_messages(&topics, &Snapshot::default()).into_iter().map(|m| m.topic));
        clear.extend(topics.retired_state_topics());
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
