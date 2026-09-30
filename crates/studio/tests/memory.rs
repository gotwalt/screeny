//! What the studio remembers about how a picture is set.
//!
//! Card 165 remembered how *each patch* was left - its working copy - so that
//! switching away and back restored it. **Card 350 moved the working copy onto
//! the channel** (`docs/design/studio-vision.md`, "Several panels": *"`patches`
//! keeps the named settings and loses the per-patch working copy to the
//! channels"*): a tuned picture is a channel's, and a tuning worth keeping
//! across a switch is a **named setting**, one library per patch for the whole
//! studio. So what these tests pin now is:
//!
//! - picking a patch puts it on its Default; a named setting brings a tuning
//!   back, on the page, on a panel and in a second browser;
//! - what each panel is showing, tuning and all, and every named setting -
//!   including for a patch that is not showing - survive a restart, in
//!   `state.json` and nowhere else;
//! - there is one library of settings, shared by every panel;
//! - a hand-edited file with garbage in it costs exactly the garbage.
//!
//! **Nothing here touches the network.** Every studio is bound to an ephemeral
//! loopback port, discovery is off (`test_config`), and the one address typed
//! at a device is an explicit `127.0.0.1` on a port nothing is listening on.

mod common;

use common::{get, post, post_as, studio, studio_in, Temp, Ws};
use std::net::SocketAddr;

/// A parameter of the current patch, as the API answers it.
fn param(state: &serde_json::Value, id: &str) -> f64 {
    state["params"][id].as_f64().unwrap_or_else(|| panic!("`{id}` in {state}"))
}

async fn ok(at: SocketAddr, path: &str, body: &str) -> serde_json::Value {
    let r = post(at, path, body).await;
    assert_eq!(r.status, 200, "{path} {body}: {}", String::from_utf8_lossy(&r.body));
    r.json()
}

async fn set_patch(at: SocketAddr, id: &str) -> serde_json::Value {
    ok(at, "/api/v1/set_patch", &format!(r#"{{"id":"{id}"}}"#)).await
}

async fn set_param(at: SocketAddr, id: &str, value: f64) -> serde_json::Value {
    ok(at, "/api/v1/set_param", &format!(r#"{{"id":"{id}","value":{value}}}"#)).await
}

async fn set_seed(at: SocketAddr, seed: u32) -> serde_json::Value {
    ok(at, "/api/v1/set_seed", &format!(r#"{{"seed":{seed}}}"#)).await
}

/// A patch picked afresh is on its Default (card 350: the working copy is the
/// channel's, not the patch's), and **a named setting is how a tuning comes
/// back** - saved once, loaded from anywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_patch_picked_again_is_on_default_and_a_setting_brings_the_tuning_back() {
    let studio = studio().await;
    let at = studio.addr;

    set_patch(at, "metaballs").await;
    set_seed(at, 111).await;
    set_param(at, "size", 2.5).await;
    let tuned = set_param(at, "hue", 12.0).await;
    assert_eq!(tuned["modified"], true);
    let saved = ok(at, "/api/v1/settings/save", r#"{"name":"Lava"}"#).await;
    assert_eq!((saved["setting"].as_str(), saved["modified"].as_bool()), (Some("Lava"), Some(false)));

    set_patch(at, "clocks-dials").await;
    let back = set_patch(at, "metaballs").await;
    assert_eq!(back["setting"], "Default", "a patch picked afresh is on its Default: {back}");
    assert_eq!(back["seed"], screeny_studio::state::DEFAULT_SEED);

    let lava = ok(at, "/api/v1/settings/load", r#"{"name":"Lava"}"#).await;
    assert_eq!(lava["seed"], 111, "the setting brings the seed back");
    assert_eq!(param(&lava, "size"), 2.5);
    assert_eq!(param(&lava, "hue"), 12.0);
    assert_eq!(lava["modified"], false);

    // `set_picture` is the same thing in one step, from any patch.
    set_patch(at, "clocks-dials").await;
    let again = ok(at, "/api/v1/set_picture", r#"{"patch":"metaballs","setting":"Lava"}"#).await;
    assert_eq!((again["patch"].as_str(), again["seed"].as_u64()), (Some("metaballs"), Some(111)));
    assert_eq!(param(&again, "size"), 2.5);
}

/// The loaded values have to reach the sliders: the browser that asked redraws
/// from the answer, and every *other* browser from the state the server pushes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_browser_sees_the_loaded_values() {
    let studio = studio().await;
    let at = studio.addr;
    let mut bob = Ws::connect(at, Some("bob")).await;
    bob.event("state").await; // the hello

    post_as(at, "/api/v1/set_patch", r#"{"id":"metaballs"}"#, Some("alice")).await;
    bob.event("state").await;
    post_as(at, "/api/v1/set_param", r#"{"id":"size","value":2.5}"#, Some("alice")).await;
    bob.event("state").await;
    post_as(at, "/api/v1/settings/save", r#"{"name":"Big"}"#, Some("alice")).await;
    bob.event("state").await;
    post_as(at, "/api/v1/set_patch", r#"{"id":"clocks-dials"}"#, Some("alice")).await;
    bob.event("state").await;

    post_as(at, "/api/v1/set_picture", r#"{"patch":"metaballs","setting":"Big"}"#, Some("alice")).await;
    let ev = bob.event("state").await;
    assert_eq!(ev["state"]["patch"], "metaballs");
    assert_eq!(param(&ev["state"], "size"), 2.5, "the push carries the loaded value: {ev}");

    // And a browser that arrives afterwards is handed the same thing.
    let mut late = Ws::connect(at, None).await;
    let hello = late.event("state").await;
    assert_eq!(param(&hello["state"], "size"), 2.5);
    assert_eq!(get(at, "/api/v1/bootstrap").await.json()["state"]["params"]["size"], 2.5);
}

/// Reset means "back to the defaults and **stay** there".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_means_the_old_value_does_not_come_back() {
    let studio = studio().await;
    let at = studio.addr;

    set_patch(at, "metaballs").await;
    let moved_size = param(&set_param(at, "size", 2.5).await, "size");
    assert_eq!(moved_size, 2.5);
    let reset = post(at, "/api/v1/reset_params", "{}").await.json();
    let default_size = param(&reset, "size");
    assert_ne!(default_size, 2.5);

    set_patch(at, "clocks-dials").await;
    let back = set_patch(at, "metaballs").await;
    assert_eq!(param(&back, "size"), default_size, "Reset was forgotten rather than obeyed: {back}");
}

/// A fresh process on the same state directory is showing what it was
/// showing, **tuning and all** - it is the channel's - and has every named
/// setting, **including one for a patch that is not showing**. That file is
/// the container's volume, so this is what survives an image rebuild.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_picture_and_the_settings_survive_a_restart() {
    let dir = Temp::new("memory-restart");
    {
        let studio = studio_in(&dir.0, false).await;
        let at = studio.addr;
        set_patch(at, "metaballs").await;
        set_seed(at, 111).await;
        set_param(at, "size", 2.5).await;
        ok(at, "/api/v1/settings/save", r#"{"name":"Lava"}"#).await;
        // Leave it showing something else entirely, tuned and unsaved.
        set_patch(at, "clocks-dials").await;
        set_param(at, "dwell", 90.0).await;
        studio.stop().await;
    }

    // What is on disk, before anything reads it back.
    let text = std::fs::read_to_string(dir.0.join("state.json")).expect("a state file");
    let file: serde_json::Value = serde_json::from_str(&text).expect("it parses");
    assert_eq!(file["version"], screeny_studio::state::SCHEMA_VERSION, "the schema this build writes");
    assert_eq!(file["patches"]["metaballs"]["settings"]["Lava"]["params"]["size"], 2.5, "in state.json and nowhere else:\n{text}");
    assert_eq!(file["channels"][0]["patch"], "clocks-dials", "{text}");
    assert_eq!(file["channels"][0]["params"]["dwell"], 90.0, "{text}");

    let studio = studio_in(&dir.0, false).await;
    let at = studio.addr;
    let boot = get(at, "/api/v1/bootstrap").await.json();
    assert_eq!(boot["state"]["patch"], "clocks-dials", "it resumes what it was showing (card 106)");
    assert_eq!(boot["state"]["params"]["dwell"], 90.0, "tuning and all");
    assert_eq!(boot["state"]["modified"], true);

    // And the setting for the patch that was *not* showing is still there.
    let back = ok(at, "/api/v1/set_picture", r#"{"patch":"metaballs","setting":"Lava"}"#).await;
    assert_eq!(back["seed"], 111);
    assert_eq!(param(&back, "size"), 2.5, "a new process loaded a setting for a patch it was not playing: {back}");
}

/// A panel, reached the way a script reaches it. The device here is an
/// address nothing answers on, and panel output is off, so nothing leaves the
/// process: this is the configuration and nothing else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panel_is_tuned_through_player_set_and_the_page_says_so() {
    let studio = studio().await;
    let at = studio.addr;
    let added = post(at, "/api/v1/devices/add", r#"{"to":"127.0.0.1:50999","name":"bench","play":false}"#).await;
    assert_eq!(added.status, 200);
    let id = added.json()["id"].as_str().expect("an id").to_string();
    let set = |body: String| async move { ok(at, "/api/v1/player/set", &body).await };

    // Panel output off: nothing is sent anywhere.
    let off = set(format!(r#"{{"device":"{id}","on":false}}"#)).await;
    assert_eq!(off["on"], false);
    assert_eq!(off["panel"], serde_json::Value::Null, "no link while output is off");
    assert_eq!(off["channel"], 1, "and a new panel is on Channel 1 (card 353)");

    set(format!(r#"{{"device":"{id}","patch":"metaballs","seed":11}}"#)).await;
    let tuned = set(format!(r#"{{"device":"{id}","param":{{"id":"size","value":2.5}}}}"#)).await;
    assert_eq!((tuned["patch"].as_str(), tuned["seed"].as_u64()), (Some("metaballs"), Some(11)));
    assert_eq!(tuned["params"]["size"], 2.5);

    // It is the only panel, so it is the first: the page says exactly what
    // it says - the patch, the seed, the parameter and that output is off.
    let boot = get(at, "/api/v1/bootstrap").await.json();
    assert_eq!(boot["state"]["patch"], "metaballs", "the page shows the first panel: {}", boot["state"]);
    assert_eq!(boot["state"]["device"], id);
    assert_eq!(boot["state"]["seed"], 11);
    assert_eq!(boot["state"]["on"], false);
    assert_eq!(param(&boot["state"], "size"), 2.5);

    // The panel's Reset.
    let reset = set(format!(r#"{{"device":"{id}","reset_params":true}}"#)).await;
    assert!(reset["params"].as_object().expect("params").is_empty());
}

/// **One library of named settings for the whole studio**: a setting saved on
/// one panel is there for every other - and a second panel asking for that
/// picture, unmodified, joins the first one's channel (card 350's rule 1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_panels_share_one_library_of_settings() {
    let studio = studio().await;
    let at = studio.addr;
    let add = |to: &str| {
        let body = format!(r#"{{"to":"{to}","name":"bench","play":false}}"#);
        async move { post(at, "/api/v1/devices/add", &body).await.json()["id"].as_str().expect("an id").to_string() }
    };
    let first = add("127.0.0.1:50997").await;
    let second = add("127.0.0.1:50998").await;
    let set = |body: String| async move { ok(at, "/api/v1/player/set", &body).await };
    for id in [&first, &second] {
        set(format!(r#"{{"device":"{id}","on":false}}"#)).await;
    }

    // Through the page's own routes, `panel` naming the first.
    ok(at, "/api/v1/set_patch", &format!(r#"{{"id":"metaballs","panel":"{first}"}}"#)).await;
    ok(at, "/api/v1/set_param", &format!(r#"{{"id":"size","value":2.5,"panel":"{first}"}}"#)).await;
    ok(at, "/api/v1/set_seed", &format!(r#"{{"seed":505,"panel":"{first}"}}"#)).await;
    ok(at, "/api/v1/settings/save", &format!(r#"{{"name":"Big","panel":"{first}"}}"#)).await;

    // The other panel, asked for that picture, gets it - on the same channel.
    let on_the_second = set(format!(r#"{{"device":"{second}","patch":"metaballs","setting":"Big"}}"#)).await;
    assert_eq!(param(&on_the_second, "size"), 2.5);
    assert_eq!(on_the_second["seed"], 505);
    assert_eq!(on_the_second["shared_with"], serde_json::json!([first]), "one picture, one channel: {on_the_second}");
    let settings = get(at, &format!("/api/v1/bootstrap?panel={second}")).await.json()["state"]["settings"].clone();
    assert_eq!(settings, serde_json::json!(["Big"]));
}

/// *"a hand-edited state file with garbage values starts a working server with
/// defaults for exactly the garbage values"* (card 165). Card 106's rule stands
/// underneath it - a file this build cannot use is kept, not deleted - and a
/// *value* it cannot use must not make the file one of those.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hand_edited_state_file_starts_a_working_server() {
    let dir = Temp::new("memory-garbage");
    std::fs::write(
        dir.0.join("state.json"),
        format!(
            r#"{{
          "version": {},
          "panels": [ {{ "device": "", "channel": 1 }} ],
          "channels": [ {{ "id": 1, "patch": "clocks-dials", "seed": 3 }} ],
          "patches": {{
            "metaballs": {{ "settings": {{ "Lava": {{ "seed": 11, "params": {{
                "size": 2.5,
                "speed": 1e30,
                "spread": "sideways",
                "count": null,
                "samples": [1, 2],
                "nonesuch": 4.0
            }} }} }} }},
            "no-such-patch": {{ "settings": {{ "Old": {{ "seed": 4 }} }} }}
          }}
        }}"#,
            screeny_studio::state::SCHEMA_VERSION
        ),
    )
    .expect("write the hand-edited file");

    let studio = studio_in(&dir.0, false).await;
    let at = studio.addr;
    assert_eq!(get(at, "/healthz").await.status, 200, "a bad value is not a sick server");
    assert!(!dir.0.join("state.bad.json").exists(), "a bad value must not condemn the file");

    let boot = get(at, "/api/v1/bootstrap").await.json();
    assert_eq!(boot["state"]["patch"], "clocks-dials");
    let spec = boot["patches"]
        .as_array()
        .expect("patches")
        .iter()
        .find(|p| p["id"] == "metaballs")
        .expect("metaballs is on the menu")
        .clone();
    let default_of = |id: &str| {
        spec["params"].as_array().expect("params").iter().find(|p| p["id"] == id).expect("a param")["default"]
            .as_f64()
            .expect("a number")
    };
    let max_of = |id: &str| {
        spec["params"].as_array().expect("params").iter().find(|p| p["id"] == id).expect("a param")["max"]
            .as_f64()
            .expect("a number")
    };

    let tuned = ok(at, "/api/v1/set_picture", r#"{"patch":"metaballs","setting":"Lava"}"#).await;
    assert_eq!(tuned["seed"], 11, "the good half of the setting was used");
    assert_eq!(param(&tuned, "size"), 2.5, "and so was the one good value");
    assert_eq!(param(&tuned, "speed"), max_of("speed"), "out of range is clamped");
    assert_eq!(param(&tuned, "spread"), default_of("spread"), "a string is the default");
    assert_eq!(param(&tuned, "count"), default_of("count"), "a null is the default");
    assert_eq!(param(&tuned, "samples"), default_of("samples"), "an array is the default");
    assert!(tuned["params"].get("nonesuch").is_none(), "a parameter this build has not got is ignored");

    // The server says what it had to correct, and it is not a fault.
    let status = get(at, "/api/v1/status").await.json();
    assert_eq!(status["ok"], true);
    assert!(!status["state"]["repaired"].as_array().expect("repaired").is_empty(), "{status}");
}

/// A v1 file comes up with everything it had: what it was playing is on the
/// page's channel, and the file it rewrites is this build's schema.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_v1_state_file_comes_up_with_what_it_had() {
    let dir = Temp::new("memory-v1");
    std::fs::write(
        dir.0.join("state.json"),
        r#"{
  "version": 1,
  "devices": [],
  "players": [],
  "preview": {
    "piece": "metaballs",
    "seed": 4242,
    "params": { "count": 5.0, "speed": 0.5, "size": 2.5, "hue": 20.0,
                "spread": 200.0, "samples": 4.0 },
    "settings": { "levels": 64, "dither": "bayer4",
                  "limiter": { "enabled": true, "apl_cap": 0.4, "max_rise_per_s": 2.0 },
                  "panel_model": true, "codec_preview": true },
    "paused": false, "speed": 1.0, "fps": 60.0,
    "panel_on": false, "panel_to": ""
  }
}"#,
    )
    .expect("write the v1 file");

    let studio = studio_in(&dir.0, false).await;
    let at = studio.addr;
    let boot = get(at, "/api/v1/bootstrap").await.json();
    assert_eq!(boot["state"]["patch"], "metaballs", "a v1 file still resumes what it was showing");
    assert_eq!(boot["state"]["seed"], 4242);
    assert_eq!(boot["state"]["params"]["size"], 2.5);

    // The file it rewrites is this build's schema, and the v1 file was not
    // condemned. (v1 -> v3 -> v5 -> v8 in one start.)
    studio.stop().await;
    let text = std::fs::read_to_string(dir.0.join("state.json")).expect("a state file");
    assert!(!dir.0.join("state.bad.json").exists());
    let file: serde_json::Value = serde_json::from_str(&text).expect("it parses");
    assert_eq!(file["version"], screeny_studio::state::SCHEMA_VERSION);
    assert_eq!(file["channels"][0]["params"]["size"], 2.5, "{text}");
    assert!(file.get("panels").is_none(), "no panel - the picture is Channel 1's (card 353): {text}");
    assert_eq!(file["channels"][0]["id"], 1, "{text}");
}
