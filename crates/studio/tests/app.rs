//! Card 359: the studio as a Home Assistant app.
//!
//! No Home Assistant here: the Supervisor is a fake HTTP server on loopback,
//! and the ingress peer is whoever the test says it is. What is proved is the
//! studio's side of both contracts - which broker it uses when, and who it
//! lets in - and that a plain deployment (no `peers`) is untouched.

mod common;

use common::{get, post, test_config, until_json, PATIENCE};
use screeny_studio::ha::supervisor::Supervisor;
use screeny_studio::ha::MqttConfig;
use screeny_studio::{Config, Studio};
use std::net::{IpAddr, SocketAddr};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PASSWORD: &str = "password9";

/// A Supervisor that answers `GET /services/mqtt` with `body` and checks the
/// bearer token. Returns its base URL; the task lives as long as the test.
async fn fake_supervisor(status: u16, body: String) -> (String, tokio::task::JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { return };
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let ok = req.starts_with("GET /services/mqtt ") && req.contains("Authorization: Bearer dummy-token");
                let (st, b) = if ok { (status, body) } else { (401, "{}".to_string()) };
                let reply = format!("HTTP/1.1 {st} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}", b.len());
                let _ = s.write_all(reply.as_bytes()).await;
            });
        }
    });
    (base, task)
}

fn offer() -> String {
    format!(r#"{{"result":"ok","data":{{"addon":"core_mosquitto","host":"127.0.0.1","port":1,"ssl":false,"username":"ha","password":"{PASSWORD}","protocol":"3.1.1"}}}}"#)
}

fn app_config(base: &str) -> Config {
    Config { supervisor: Some(Supervisor { base: base.to_string(), token: "dummy-token".into() }), ..test_config() }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_supervisors_broker_is_used_when_nothing_is_set_and_its_password_never_leaves() {
    let (base, _fake) = fake_supervisor(200, offer()).await;
    let studio = Studio::bind(app_config(&base)).await.unwrap().spawn();
    let at = studio.addr;

    let view = until_json(at, PATIENCE, "the Supervisor's broker", "/api/v1/home_assistant", |v| v["from_supervisor"] == true).await;
    assert_eq!(view["host"], "127.0.0.1");
    assert_eq!(view["port"], 1);
    assert_eq!(view["username"], "ha");
    assert_eq!(view["enabled"], true);
    assert_eq!(view["password_set"], true);
    for path in ["/api/v1/home_assistant", "/api/v1/status", "/api/v1/bootstrap"] {
        let body = get(at, path).await.body;
        assert!(!String::from_utf8_lossy(&body).contains(PASSWORD), "{path} leaked the broker password");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owners_own_settings_beat_the_supervisors() {
    let (base, _fake) = fake_supervisor(200, offer()).await;
    let studio = Studio::bind(app_config(&base)).await.unwrap().spawn();
    let at = studio.addr;
    until_json(at, PATIENCE, "the Supervisor's broker", "/api/v1/home_assistant", |v| v["from_supervisor"] == true).await;

    // The owner types a broker of their own: it replaces the Supervisor's.
    let r = post(at, "/api/v1/home_assistant/set", r#"{"host":"broker.example","enabled":true}"#).await;
    assert_eq!(r.status, 200);
    let v = r.json();
    assert_eq!((v["from_supervisor"].clone(), v["host"].as_str()), (false.into(), Some("broker.example")));

    // ...and an explicit "off" stays off, rather than falling back to the
    // Supervisor's broker.
    let v = post(at, "/api/v1/home_assistant/set", r#"{"enabled":false}"#).await.json();
    assert_eq!((v["from_supervisor"].clone(), v["enabled"].clone()), (false.into(), false.into()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_already_saved_win_from_the_first_moment() {
    let (base, _fake) = fake_supervisor(200, offer()).await;
    let mine = MqttConfig {
        host: "mine.example".into(),
        port: 1883,
        username: None,
        password: None,
        discovery_prefix: "homeassistant".into(),
        instance: "studio".into(),
        name: "Screeny".into(),
    };
    let studio = Studio::bind(Config { mqtt: Some(mine), ..app_config(&base) }).await.unwrap().spawn();
    // Give the Supervisor poller time to have answered.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let v = get(studio.addr, "/api/v1/home_assistant").await.json();
    assert_eq!((v["from_supervisor"].clone(), v["host"].as_str()), (false.into(), Some("mine.example")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_mosquitto_yet_is_not_a_failure() {
    let (base, _fake) = fake_supervisor(400, r#"{"result":"error","message":"no service"}"#.into()).await;
    let studio = Studio::bind(app_config(&base)).await.unwrap().spawn();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let v = get(studio.addr, "/api/v1/home_assistant").await.json();
    assert_eq!((v["from_supervisor"].clone(), v["enabled"].clone(), v["status"]["state"].as_str()), (false.into(), false.into(), Some("off")));
    assert_eq!(get(studio.addr, "/healthz").await.status, 200, "no broker is never a reason to restart");
}

/// The peer rule: in app mode only the listed peers are served, 403 for the
/// rest, over HTTP and the websocket's upgrade alike; a plain studio (no list)
/// serves everybody. `GET /healthz` is open to any peer: the image's
/// HEALTHCHECK is not the Supervisor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_named_peer_is_served_in_app_mode() {
    let stranger: IpAddr = "10.9.9.9".parse().unwrap();
    let studio = Studio::bind(Config { peers: Some(vec![stranger]), ..test_config() }).await.unwrap().spawn();
    assert_eq!(get(studio.addr, "/healthz").await.status, 200, "the container's own healthcheck");
    for path in ["/", "/api/v1/status", "/api/v1/ws"] {
        assert_eq!(get(studio.addr, path).await.status, 403, "{path} from a peer that is not the Supervisor");
    }
    assert_eq!(post(studio.addr, "/api/v1/home_assistant/set", "{}").await.status, 403);
    drop(studio);

    let me: IpAddr = "127.0.0.1".parse().unwrap();
    let studio = Studio::bind(Config { peers: Some(vec![stranger, me]), ..test_config() }).await.unwrap().spawn();
    assert_eq!(get(studio.addr, "/").await.status, 200);
    assert_eq!(get(studio.addr, "/healthz").await.status, 200);
    drop(studio);

    let studio = Studio::bind(test_config()).await.unwrap().spawn();
    assert_eq!(get(studio.addr, "/").await.status, 200, "a plain deployment has no peer check");
}

/// `direct_access`: a second listener that is not peer-checked, beside the
/// checked one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_access_is_a_second_listener_without_the_check() {
    let stranger: IpAddr = "10.9.9.9".parse().unwrap();
    let cfg = Config { peers: Some(vec![stranger]), direct: Some(SocketAddr::from(([127, 0, 0, 1], 0))), ..test_config() };
    let studio = Studio::bind(cfg).await.unwrap();
    let direct = studio.direct_addr.expect("a direct listener");
    let running = studio.spawn();
    assert_eq!(get(running.addr, "/").await.status, 403, "ingress stays checked");
    assert_eq!(get(direct, "/").await.status, 200, "direct is open, as asked");
    assert_eq!(get(direct, "/healthz").await.status, 200);
    running.stop().await;
}
