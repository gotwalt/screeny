//! Card 367: export the Studio's settings from one Studio and import them into
//! another (moving from a docker Studio to the Home Assistant app).
//!
//! No panel is reached: panels here are device ids with nothing at the other
//! end, which is also what a freshly imported Studio has until it finds them.

mod common;

use common::{get, post, studio_in, test_config, until_json, Temp, PATIENCE};
use screeny_studio::ha::supervisor::Supervisor;
use screeny_studio::{Config, Studio};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PASSWORD: &str = "password9";

/// A v10 state file as a docker Studio would have left it: two panels on two
/// channels, a device a human typed an address for, a setting with a
/// tuned parameter, a non-default output, and a broker with a password.
fn old_studio_state() -> String {
    serde_json::json!({
        "version": 10,
        "devices": [
            {"id": "bb0001", "name": "Office", "instance": "screeny-bb0001", "address": "192.0.2.41:49374", "manual": true},
            {"id": "bb0002", "name": "Hall", "address": "192.0.2.42"},
            {"id": "pending:192.0.2.9", "name": "Never answered", "address": "192.0.2.9", "manual": true}
        ],
        "panels": [
            {"device": "bb0001", "on": true, "channel": 1, "brightness": 40},
            {"device": "bb0002", "on": false, "channel": 2},
            {"device": "pending:192.0.2.9", "on": true, "channel": 1}
        ],
        "channels": [
            {"id": 1, "name": "Living room", "patch": "metaballs", "seed": 7, "params": {"size": 2.5}},
            {"id": 2, "name": "Hall", "patch": "clocks-dials", "params": {"dwell": 90.0}}
        ],
        "output": {"dither": "bayer8"},
        "patches": {
            "metaballs": {"settings": {"Lava": {"seed": 111, "params": {"size": 2.5}, "speed": 1.0}}}
        },
        "home_assistant": {
            "enabled": true, "host": "broker.old.example", "port": 1883,
            "username": "old", "password": PASSWORD
        }
    })
    .to_string()
}

async fn old_studio(tag: &str) -> (Temp, screeny_studio::Running) {
    let dir = Temp::new(tag);
    std::fs::write(dir.0.join("state.json"), old_studio_state()).expect("write the old state");
    let studio = studio_in(&dir.0, false).await;
    (dir, studio)
}

/// What a person would compare after a move: panels, channels, output, the
/// settings of `metaballs`.
async fn picture_of(at: std::net::SocketAddr) -> serde_json::Value {
    let panels = get(at, "/api/v1/panels").await.json();
    let channels = get(at, "/api/v1/channels").await.json();
    let ch2 = get(at, "/api/v1/bootstrap?channel=2").await.json();
    let ch1 = get(at, "/api/v1/bootstrap").await.json();
    let slim = |p: &serde_json::Value| {
        serde_json::json!({
            "device": p["device"], "name": p["name"], "on": p["on"], "brightness": p["brightness"],
            "channel": p["channel"], "channel_name": p["channel_name"],
        })
    };
    serde_json::json!({
        "panels": panels["panels"].as_array().unwrap().iter().filter(|p| !p["device"].as_str().unwrap_or_default().starts_with("pending")).map(slim).collect::<Vec<_>>(),
        "channels": channels["channels"].as_array().unwrap().iter()
            .map(|c| serde_json::json!({"id": c["id"], "name": c["name"], "patch": c["picture"]["patch"], "setting": c["picture"]["setting"], "output": c["output"]}))
            .collect::<Vec<_>>(),
        "ch1": {"seed": ch1["state"]["seed"], "params": ch1["state"]["params"], "settings": ch1["state"]["settings"]},
        "ch2": {"seed": ch2["state"]["seed"], "params": ch2["state"]["params"]},
    })
}

fn state_file(dir: &Temp) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(dir.0.join("state.json")).expect("a state file")).expect("it parses")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_export_is_a_download_with_no_password_and_no_addresses() {
    let (_dir, studio) = old_studio("export-shape").await;
    let r = get(studio.addr, "/api/v1/state/export").await;
    assert_eq!(r.status, 200);
    let head = r.head.to_lowercase();
    assert!(head.contains("content-disposition: attachment; filename=\"screeny-studio-"), "{}", r.head);
    assert!(head.contains(".json\""), "{}", r.head);
    let text = String::from_utf8(r.body.clone()).unwrap();
    let doc = r.json();
    assert_eq!(doc["export"], "screeny-studio");
    assert_eq!(doc["version"], screeny_studio::state::SCHEMA_VERSION);
    assert!(doc.get("home_assistant").is_none(), "{text}");
    for secret in [PASSWORD, "broker.old.example", "192.0.2.", "screeny-bb0001", "pending"] {
        assert!(!text.contains(secret), "the export leaked `{secret}`:\n{text}");
    }
    assert_eq!(doc["devices"], serde_json::json!([{"id": "bb0001", "name": "Office"}, {"id": "bb0002", "name": "Hall"}]));
    assert_eq!(doc["panels"].as_array().unwrap().len(), 2, "the provisional device's panel does not travel: {text}");
    assert_eq!(doc["channels"].as_array().unwrap().len(), 2);
    assert_eq!(doc["patches"]["metaballs"]["settings"]["Lava"]["seed"], 111);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_round_trip_gives_the_same_panels_channels_and_settings() {
    let (_dir_a, a) = old_studio("roundtrip-a").await;
    let export = get(a.addr, "/api/v1/state/export").await;
    assert_eq!(export.status, 200);
    let before = picture_of(a.addr).await;

    let dir_b = Temp::new("roundtrip-b");
    let b = studio_in(&dir_b.0, false).await;
    assert_ne!(picture_of(b.addr).await, before, "a fresh Studio is not the old one");
    let r = post(b.addr, "/api/v1/state/import", &String::from_utf8(export.body).unwrap()).await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    let answer = r.json();
    assert_eq!((answer["applied"].as_str(), answer["panels"].as_u64(), answer["channels"].as_u64()), (Some("live"), Some(2), Some(2)), "{answer}");

    // Live: no restart, and the picture is the old Studio's.
    let after = picture_of(b.addr).await;
    assert_eq!(after, before);
    assert_eq!(after["panels"][0]["name"], "Office");
    assert_eq!(after["panels"][1]["on"], false);
    assert_eq!(after["channels"][1]["name"], "Hall");
    assert_eq!(after["ch1"]["seed"], 7);
    assert_eq!(after["channels"][0]["output"]["dither"], "bayer8");

    // And saved: it is what a restart comes back to.
    b.stop().await;
    let file = state_file(&dir_b);
    assert_eq!(file["patches"]["metaballs"]["settings"]["Lava"]["params"]["size"], 2.5);
    assert_eq!(file["channels"][1]["patch"], "clocks-dials");
    let b = studio_in(&dir_b.0, false).await;
    assert_eq!(picture_of(b.addr).await, before, "a restart after the import");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_address_is_trusted_and_the_importing_studios_own_are_kept() {
    let (_dir_a, a) = old_studio("addr-a").await;
    let export = String::from_utf8(get(a.addr, "/api/v1/state/export").await.body).unwrap();
    // A hand-edited file that tries to bring addresses along anyway.
    let mut doc: serde_json::Value = serde_json::from_str(&export).unwrap();
    doc["devices"][0]["address"] = "192.0.2.99".into();
    doc["devices"][0]["instance"] = "elsewhere".into();
    doc["devices"][0]["manual"] = true.into();

    // The new Studio already found bb0001 itself, at its own place.
    let dir_b = Temp::new("addr-b");
    std::fs::write(
        dir_b.0.join("state.json"),
        serde_json::json!({"version": 10, "devices": [{"id": "bb0001", "name": "old name", "instance": "mine", "address": "192.0.2.77"}]}).to_string(),
    )
    .unwrap();
    let b = studio_in(&dir_b.0, false).await;
    assert_eq!(post(b.addr, "/api/v1/state/import", &doc.to_string()).await.status, 200);
    b.stop().await;
    let file = state_file(&dir_b);
    let dev = |id: &str| file["devices"].as_array().unwrap().iter().find(|d| d["id"] == id).cloned().unwrap_or_default();
    let known = dev("bb0001");
    assert_eq!(known["name"], "Office", "the name travels");
    assert_eq!((known["address"].as_str(), known["instance"].as_str()), (Some("192.0.2.77"), Some("mine")), "the new Studio's own: {file}");
    let fresh = dev("bb0002");
    assert_eq!(fresh["name"], "Hall");
    for k in ["address", "instance", "manual"] {
        let v = &fresh[k];
        assert!(v.is_null() || v == "" || v == false, "an imported device has no {k}: {fresh}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_older_schema_export_imports_through_the_migrations() {
    let b = common::studio().await;
    // A v7 export: one player per device, `piece` before it was `patch`.
    let old = serde_json::json!({
        "export": "screeny-studio",
        "version": 7,
        "devices": [{"id": "bb0001", "name": "Desk", "address": "192.0.2.5"}],
        "players": [{"device": "bb0001", "on": true, "piece": "clocks-dials", "seed": 5, "params": {"dwell": 30.0}}],
        "pieces": {"metaballs": {"settings": {"Calm": {"seed": 3, "params": {"size": 1.5}, "speed": 1.0}}}}
    });
    let r = post(b.addr, "/api/v1/state/import", &old.to_string()).await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(r.json()["from_version"], 7);
    let panels = get(b.addr, "/api/v1/panels").await.json();
    let p = &panels["panels"][0];
    assert_eq!((p["device"].as_str(), p["name"].as_str(), p["picture"]["patch"].as_str()), (Some("bb0001"), Some("Desk"), Some("clocks-dials")), "{panels}");
    let state = get(b.addr, &format!("/api/v1/bootstrap?channel={}", p["channel"])).await.json();
    assert_eq!((state["state"]["seed"].as_u64(), state["state"]["params"]["dwell"].as_f64()), (Some(5), Some(30.0)), "{state}");
    // The named setting came with it.
    let on = format!(r#"{{"id":"metaballs","channel":{}}}"#, p["channel"]);
    assert_eq!(post(b.addr, "/api/v1/set_patch", &on).await.status, 200);
    let r = post(b.addr, "/api/v1/settings/load", &format!(r#"{{"name":"Calm","channel":{}}}"#, p["channel"])).await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(r.json()["seed"], 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_file_changes_nothing() {
    let (dir, studio) = old_studio("bad").await;
    let at = studio.addr;
    let before = picture_of(at).await;
    let file_before = state_file(&dir);
    let good = String::from_utf8(get(at, "/api/v1/state/export").await.body).unwrap();

    let mut future: serde_json::Value = serde_json::from_str(&good).unwrap();
    future["version"] = 11.into();
    let mut no_marker: serde_json::Value = serde_json::from_str(&good).unwrap();
    no_marker.as_object_mut().unwrap().remove("export");
    let mut no_version: serde_json::Value = serde_json::from_str(&good).unwrap();
    no_version.as_object_mut().unwrap().remove("version");
    let mut wrong_shape: serde_json::Value = serde_json::from_str(&good).unwrap();
    wrong_shape["panels"] = "not a list".into();
    let mut huge: serde_json::Value = serde_json::from_str(&good).unwrap();
    huge["channels"] = (1..=300).map(|i| serde_json::json!({"id": i})).collect::<Vec<_>>().into();

    let bads = [
        ("not json", "this is not json".to_string()),
        ("an array", "[1, 2]".to_string()),
        ("no marker", no_marker.to_string()),
        ("no version", no_version.to_string()),
        ("a newer schema", future.to_string()),
        ("the wrong shape", wrong_shape.to_string()),
        ("too many channels", huge.to_string()),
        ("a bare state.json", old_studio_state()),
    ];
    for (what, body) in bads {
        let r = post(at, "/api/v1/state/import", &body).await;
        assert_eq!(r.status, 400, "{what}: {}", String::from_utf8_lossy(&r.body));
        assert!(r.json()["error"].as_str().is_some_and(|e| !e.is_empty()), "{what}: says why");
        assert_eq!(picture_of(at).await, before, "{what} changed the live Studio");
    }
    studio.stop().await;
    let after = state_file(&dir);
    for (was, now) in file_before["channels"].as_array().unwrap().iter().zip(after["channels"].as_array().unwrap()) {
        assert_eq!((&was["name"], &was["patch"], &was["params"]), (&now["name"], &now["patch"], &now["params"]), "nor the file");
    }
    assert_eq!(after["patches"], file_before["patches"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_importing_studios_own_broker_is_kept() {
    let (_dir_a, a) = old_studio("keep-broker-a").await;
    let export = String::from_utf8(get(a.addr, "/api/v1/state/export").await.body).unwrap();
    let mut doc: serde_json::Value = serde_json::from_str(&export).unwrap();
    // Even a file that has a broker block (hand-edited in) does not move it.
    doc["home_assistant"] = serde_json::json!({"enabled": true, "host": "broker.old.example", "password": PASSWORD});

    let dir_b = Temp::new("keep-broker-b");
    let b = studio_in(&dir_b.0, false).await;
    let set = post(b.addr, "/api/v1/home_assistant/set", r#"{"host":"broker.mine.example","username":"me","password":"password8","enabled":false}"#).await;
    assert_eq!(set.status, 200);
    assert_eq!(post(b.addr, "/api/v1/state/import", &doc.to_string()).await.status, 200);
    let view = get(b.addr, "/api/v1/home_assistant").await.json();
    assert_eq!((view["host"].as_str(), view["username"].as_str(), view["enabled"].clone(), view["password_set"].clone()), (Some("broker.mine.example"), Some("me"), false.into(), true.into()), "{view}");
    b.stop().await;
    let file = state_file(&dir_b);
    assert_eq!(file["home_assistant"]["host"], "broker.mine.example");
    assert_eq!(file["home_assistant"]["password"], "password8");
}

/// A Supervisor that answers `GET /services/mqtt`, as `tests/app.rs` has it.
async fn fake_supervisor(body: String) -> (String, tokio::task::JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { return };
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let _ = s.read(&mut buf).await;
                let reply = format!("HTTP/1.1 200 X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = s.write_all(reply.as_bytes()).await;
            });
        }
    });
    (base, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_app_mode_the_supervisors_broker_survives_an_import() {
    let offer = r#"{"result":"ok","data":{"addon":"core_mosquitto","host":"127.0.0.1","port":1,"ssl":false,"username":"ha","password":"password7","protocol":"3.1.1"}}"#;
    let (base, _fake) = fake_supervisor(offer.to_string()).await;
    let cfg = Config { supervisor: Some(Supervisor { base, token: "dummy-token".into() }), ..test_config() };
    let app = Studio::bind(cfg).await.unwrap().spawn();
    until_json(app.addr, PATIENCE, "the Supervisor's broker", "/api/v1/home_assistant", |v| v["from_supervisor"] == true).await;

    let (_dir_a, a) = old_studio("app-a").await;
    let export = String::from_utf8(get(a.addr, "/api/v1/state/export").await.body).unwrap();
    let r = post(app.addr, "/api/v1/state/import", &export).await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));

    let view = until_json(app.addr, PATIENCE, "the Supervisor's broker after the import", "/api/v1/home_assistant", |v| v["from_supervisor"] == true).await;
    assert_eq!((view["host"].as_str(), view["username"].as_str()), (Some("127.0.0.1"), Some("ha")), "{view}");
    assert_ne!(view["host"], "broker.old.example");
    let text = String::from_utf8_lossy(&get(app.addr, "/api/v1/home_assistant").await.body).to_string();
    assert!(!text.contains(PASSWORD) && !text.contains("password7"), "{text}");
    // ...and the picture did come across.
    assert_eq!(get(app.addr, "/api/v1/panels").await.json()["panels"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn importing_over_a_studio_that_is_running_replaces_what_it_had_live() {
    let (_dir_a, a) = old_studio("over-a").await;
    let export = String::from_utf8(get(a.addr, "/api/v1/state/export").await.body).unwrap();

    let (b, st) = common::studio_and_state().await;
    let at = b.addr;
    // B has channels of its own, one of them with a panel the file does not
    // know on it.
    assert_eq!(post(at, "/api/v1/channels/new", r#"{"name":"Mine"}"#).await.status, 200);
    assert_eq!(post(at, "/api/v1/channels/new", r#"{"name":"Spare"}"#).await.status, 200);
    st.panels.adopt("cc0003");
    assert_eq!(post(at, "/api/v1/panel/channel", r#"{"panel":"cc0003","channel":3}"#).await.status, 200);

    assert_eq!(post(at, "/api/v1/state/import", &export).await.status, 200);
    let channels = get(at, "/api/v1/channels").await.json();
    let names: Vec<_> = channels["channels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["id"].as_u64().unwrap(), c["name"].as_str().unwrap().to_string(), c["picture"]["patch"].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(names, vec![(1, "Living room".into(), "metaballs".into()), (2, "Hall".into(), "clocks-dials".into())], "Spare is gone, Mine became Hall");
    let panels = get(at, "/api/v1/panels").await.json();
    let on = |id: &str| panels["panels"].as_array().unwrap().iter().find(|p| p["device"] == id).map(|p| p["channel"].as_u64().unwrap());
    assert_eq!(on("bb0001"), Some(1));
    assert_eq!(on("bb0002"), Some(2));
    assert_eq!(on("cc0003"), Some(1), "a panel the file does not name stays, on Channel 1 now that its channel went");
}
