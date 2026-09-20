//! Card 165: the studio remembers how each patch was left.
//!
//! Tune a patch, go and tune another, come back, and it is as you left it -
//! in the design view, on a panel, in a second browser, and after the process
//! has been restarted. The memory lives in `state.json` in the state
//! directory and nowhere else, which is what makes it survive a deploy: the
//! container mounts a volume there and nothing else in this file is kept.
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

async fn set_patch(at: SocketAddr, id: &str) -> serde_json::Value {
    let r = post(at, "/api/v1/set_patch", &format!(r#"{{"id":"{id}"}}"#)).await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    r.json()
}

async fn set_param(at: SocketAddr, id: &str, value: f64) -> serde_json::Value {
    let r = post(at, "/api/v1/set_param", &format!(r#"{{"id":"{id}","value":{value}}}"#)).await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    r.json()
}

async fn set_seed(at: SocketAddr, seed: u32) -> serde_json::Value {
    post(at, "/api/v1/set_seed", &format!(r#"{{"seed":{seed}}}"#)).await.json()
}

/// The owner's request, in the design view, in his own words (card 150 leaves
/// them as he said them): *"changing settings for a given art piece persists
/// the settings so that if we switch pieces and then switch back, it restores
/// the settings"*.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switching_away_and_back_restores_the_settings() {
    let studio = studio().await;
    let at = studio.addr;

    set_patch(at, "plasma").await;
    set_seed(at, 111).await;
    set_param(at, "scale", 2.5).await;
    let plasma_default_drift = param(&set_param(at, "hue", 12.0).await, "drift");

    set_patch(at, "metaballs").await;
    set_seed(at, 222).await;
    let away = set_param(at, "count", 8.0).await;
    assert!(away["params"].get("scale").is_none(), "the other patch's parameters do not come along");

    let back = set_patch(at, "plasma").await;
    assert_eq!(back["patch"], "plasma");
    assert_eq!(back["seed"], 111, "and on the seed it was left on");
    assert_eq!(param(&back, "scale"), 2.5);
    assert_eq!(param(&back, "hue"), 12.0);
    assert_eq!(param(&back, "drift"), plasma_default_drift, "an untouched parameter is still its default");

    // And the patch we went to is still where *it* was left.
    let other = set_patch(at, "metaballs").await;
    assert_eq!(other["seed"], 222);
    assert_eq!(param(&other, "count"), 8.0);
}

/// The restored values have to reach the sliders. The design view redraws from
/// what `set_patch` answers, and every *other* browser redraws from the state
/// change the server pushes - so both paths carry them or only one browser is
/// right.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_browser_sees_the_restored_values() {
    let studio = studio().await;
    let at = studio.addr;
    let mut bob = Ws::connect(at, Some("bob")).await;
    bob.event("state").await; // the hello

    post_as(at, "/api/v1/set_patch", r#"{"id":"plasma"}"#, Some("alice")).await;
    bob.event("state").await;
    post_as(at, "/api/v1/set_param", r#"{"id":"scale","value":2.5}"#, Some("alice")).await;
    bob.event("state").await;
    post_as(at, "/api/v1/set_patch", r#"{"id":"metaballs"}"#, Some("alice")).await;
    bob.event("state").await;

    // Alice switches back. Bob is told, and what he is told is the restored
    // value - not the default he would draw if he rebuilt from the patch spec.
    post_as(at, "/api/v1/set_patch", r#"{"id":"plasma"}"#, Some("alice")).await;
    let ev = bob.event("state").await;
    assert_eq!(ev["state"]["patch"], "plasma");
    assert_eq!(param(&ev["state"], "scale"), 2.5, "the push carries the restored value: {ev}");

    // And a browser that arrives afterwards is handed the same thing.
    let mut late = Ws::connect(at, None).await;
    let hello = late.event("state").await;
    assert_eq!(param(&hello["state"], "scale"), 2.5);
    assert_eq!(get(at, "/api/v1/bootstrap").await.json()["state"]["params"]["scale"], 2.5);
}

/// Reset means "back to the defaults and **stay** there", so the old value
/// must not be handed straight back on the next switch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_means_the_old_value_does_not_come_back() {
    let studio = studio().await;
    let at = studio.addr;

    set_patch(at, "plasma").await;
    let default_scale = param(&set_param(at, "scale", 2.5).await, "scale");
    assert_eq!(default_scale, 2.5);
    let reset = post(at, "/api/v1/reset_params", "{}").await.json();
    let default_scale = param(&reset, "scale");
    assert_ne!(default_scale, 2.5);

    set_patch(at, "metaballs").await;
    let back = set_patch(at, "plasma").await;
    assert_eq!(param(&back, "scale"), default_scale, "Reset was forgotten rather than obeyed: {back}");
}

/// The memory is in the state file, so a fresh process on the same state
/// directory has it - **including for a patch that is not the one showing**,
/// which is the half that a "resumes what it was playing" test would miss.
/// That file is the container's volume, so this is also what makes the memory
/// survive an image rebuild.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_memory_survives_a_restart_including_a_patch_that_is_not_showing() {
    let dir = Temp::new("memory-restart");
    {
        let studio = studio_in(&dir.0, false).await;
        let at = studio.addr;
        set_patch(at, "plasma").await;
        set_seed(at, 111).await;
        set_param(at, "scale", 2.5).await;
        // Leave it showing something else entirely.
        set_patch(at, "metaballs").await;
        set_param(at, "count", 8.0).await;
        studio.stop().await;
    }

    // What is on disk, before anything reads it back.
    let text = std::fs::read_to_string(dir.0.join("state.json")).expect("a state file");
    let file: serde_json::Value = serde_json::from_str(&text).expect("it parses");
    assert_eq!(file["version"], screeny_studio::state::SCHEMA_VERSION, "the schema this build writes");
    assert_eq!(file["patches"]["plasma"]["params"]["scale"], 2.5, "in state.json and nowhere else:\n{text}");
    assert_eq!(file["patches"]["plasma"]["seed"], 111);

    let studio = studio_in(&dir.0, false).await;
    let at = studio.addr;
    let boot = get(at, "/api/v1/bootstrap").await.json();
    assert_eq!(boot["state"]["patch"], "metaballs", "it resumes what it was showing (card 106)");
    assert_eq!(boot["state"]["params"]["count"], 8.0);

    // And the patch that was *not* showing is still as it was left.
    let back = set_patch(at, "plasma").await;
    assert_eq!(back["seed"], 111);
    assert_eq!(param(&back, "scale"), 2.5, "a new process restored a patch it was not playing: {back}");
}

/// A panel's player restores it the same way, reached the way a script
/// reaches it. The device here is an address nothing answers on, and panel
/// output is off, so nothing leaves the process: this is the configuration and
/// nothing else.
///
/// The second half is card 170's: that panel is the one the page is a window
/// onto, so the page says exactly the same thing. Before this card the two
/// were different contexts and this test had to say so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panel_restores_a_patchs_memory_too() {
    let studio = studio().await;
    let at = studio.addr;
    let added = post(at, "/api/v1/devices/add", r#"{"to":"127.0.0.1:50999","name":"bench","play":false}"#).await;
    assert_eq!(added.status, 200);
    let id = added.json()["id"].as_str().expect("an id").to_string();
    let set = |body: String| async move { post(at, "/api/v1/player/set", &body).await };

    // Panel output off: nothing is sent anywhere. The picture carries on,
    // because the page is still showing it.
    let off = set(format!(r#"{{"device":"{id}","on":false}}"#)).await;
    assert_eq!(off.status, 200, "{}", String::from_utf8_lossy(&off.body));
    assert_eq!(off.json()["on"], false);
    assert_eq!(off.json()["panel"], serde_json::Value::Null, "no link while output is off");

    set(format!(r#"{{"device":"{id}","patch":"plasma","seed":11}}"#)).await;
    set(format!(r#"{{"device":"{id}","param":{{"id":"scale","value":2.5}}}}"#)).await;
    set(format!(r#"{{"device":"{id}","patch":"metaballs","seed":22}}"#)).await;
    let away = set(format!(r#"{{"device":"{id}","param":{{"id":"count","value":8.0}}}}"#)).await.json();
    assert!(away["params"].get("scale").is_none());

    let back = set(format!(r#"{{"device":"{id}","patch":"plasma"}}"#)).await.json();
    assert_eq!(back["seed"], 11);
    assert_eq!(param(&back, "scale"), 2.5);

    // One panel, one picture: the page is a window onto that player, so what
    // it says is what the panel says - the patch, the seed, the parameter and
    // the fact that output is off.
    let boot = get(at, "/api/v1/bootstrap").await.json();
    assert_eq!(boot["state"]["patch"], "plasma", "the page shows the attached panel: {}", boot["state"]);
    assert_eq!(boot["state"]["device"], id);
    assert_eq!(boot["state"]["seed"], 11);
    assert_eq!(boot["state"]["on"], false);
    assert_eq!(param(&boot["state"], "scale"), 2.5);

    // The player's Reset, and it stays reset.
    let reset = set(format!(r#"{{"device":"{id}","reset_params":true}}"#)).await.json();
    assert!(reset["params"].as_object().expect("params").is_empty());
    set(format!(r#"{{"device":"{id}","patch":"metaballs"}}"#)).await;
    let after = set(format!(r#"{{"device":"{id}","patch":"plasma"}}"#)).await.json();
    assert!(after["params"].as_object().expect("params").is_empty(), "Reset on a panel means it stays reset: {after}");
}

/// **One memory for the whole studio**, which is what card 165 settled once
/// the orchestrator reversed its per-context decision. With card 170 the page
/// *is* one of the panels, so the interesting version of this is two panels:
/// tuning a patch on one is tuning it everywhere.
///
/// (This replaces `promoting_the_preview_needs_no_copy_step` and
/// `the_design_view_and_the_panel_share_one_memory`. Both were about the
/// preview being a context of its own, which it no longer is; what they were
/// really pinning - one memory, no copy step - is here.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_panels_share_the_one_memory() {
    let studio = studio().await;
    let at = studio.addr;
    let add = |to: &str| {
        let body = format!(r#"{{"to":"{to}","name":"bench","play":false}}"#);
        async move { post(at, "/api/v1/devices/add", &body).await.json()["id"].as_str().expect("an id").to_string() }
    };
    let first = add("127.0.0.1:50997").await;
    let second = add("127.0.0.1:50998").await;
    let set = |body: String| async move { post(at, "/api/v1/player/set", &body).await };
    for id in [&first, &second] {
        set(format!(r#"{{"device":"{id}","on":false}}"#)).await;
    }

    // The page is a window onto the first one, so tuning it through the
    // design view's own routes is tuning that panel.
    set_patch(at, "plasma").await;
    set_param(at, "scale", 2.5).await;
    set_seed(at, 505).await;
    let on_the_first = set(format!(r#"{{"device":"{first}","patch":"plasma"}}"#)).await.json();
    assert_eq!(param(&on_the_first, "scale"), 2.5, "the page and the panel it shows are one player");

    // The *other* panel, asked for that patch, plays it the same way: there is
    // one answer to "how is plasma set", not one per panel.
    let on_the_second = set(format!(r#"{{"device":"{second}","patch":"plasma"}}"#)).await.json();
    assert_eq!(param(&on_the_second, "scale"), 2.5);
    assert_eq!(on_the_second["seed"], 505);

    // And the other way round: tune it there, and the page follows on the
    // next switch back.
    set(format!(r#"{{"device":"{second}","param":{{"id":"scale","value":0.6}}}}"#)).await;
    set_patch(at, "metaballs").await;
    let back = set_patch(at, "plasma").await;
    assert_eq!(param(&back, "scale"), 0.6, "one memory, one answer: {back}");
}

/// The card's second acceptance, end to end: *"a hand-edited state file with
/// garbage values starts a working server with defaults for exactly the garbage
/// values"*. Card 106's rule stands underneath it - a file this build cannot
/// use is kept, not deleted - and a *value* it cannot use must not make the
/// file one of those.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hand_edited_state_file_starts_a_working_server() {
    let dir = Temp::new("memory-garbage");
    std::fs::write(
        dir.0.join("state.json"),
        r#"{
          "version": 2,
          "preview": { "piece": "metaballs" },
          "pieces": {
            "plasma": { "seed": 11, "params": {
                "scale": 2.5,
                "drift": 1e30,
                "cycle": "sideways",
                "bands": null,
                "colours": [1, 2],
                "nonesuch": 4.0
            } },
            "no-such-piece": { "seed": 4 }
          }
        }"#,
    )
    .expect("write the hand-edited file");

    let studio = studio_in(&dir.0, false).await;
    let at = studio.addr;
    assert_eq!(get(at, "/healthz").await.status, 200, "a bad value is not a sick server");
    assert!(!dir.0.join("state.bad.json").exists(), "a bad value must not condemn the file");

    let boot = get(at, "/api/v1/bootstrap").await.json();
    assert_eq!(boot["state"]["patch"], "metaballs");
    let spec = boot["patches"]
        .as_array()
        .expect("patches")
        .iter()
        .find(|p| p["id"] == "plasma")
        .expect("plasma is on the menu")
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

    let plasma = set_patch(at, "plasma").await;
    assert_eq!(plasma["seed"], 11, "the good half of the entry was used");
    assert_eq!(param(&plasma, "scale"), 2.5, "and so was the one good value");
    assert_eq!(param(&plasma, "drift"), max_of("drift"), "out of range is clamped");
    assert_eq!(param(&plasma, "cycle"), default_of("cycle"), "a string is the default");
    assert_eq!(param(&plasma, "bands"), default_of("bands"), "a null is the default");
    assert_eq!(param(&plasma, "colours"), default_of("colours"), "an array is the default");
    assert!(plasma["params"].get("nonesuch").is_none(), "a parameter this build has not got is ignored");

    // The server says what it had to correct, and it is not a fault.
    let status = get(at, "/api/v1/status").await.json();
    assert_eq!(status["ok"], true);
    assert!(!status["state"]["repaired"].as_array().expect("repaired").is_empty(), "{status}");
}

/// A v1 file - what the deployed service has today - comes up with everything
/// it had, and what it was playing is now that patch's memory.
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
    "piece": "plasma",
    "seed": 4242,
    "params": { "scale": 2.5, "drift": 0.35, "cycle": 0.12, "bands": 1.5, "colours": 32.0,
                "black": 0.45, "hue": 300.0, "spread": 140.0, "dither": 1.0 },
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
    assert_eq!(boot["state"]["patch"], "plasma", "a v1 file still resumes what it was showing");
    assert_eq!(boot["state"]["seed"], 4242);
    assert_eq!(boot["state"]["params"]["scale"], 2.5);

    // And the tuning it had is now remembered: go away and come back.
    set_patch(at, "metaballs").await;
    let back = set_patch(at, "plasma").await;
    assert_eq!(back["seed"], 4242, "what v1 was playing became that patch's first memory");
    assert_eq!(param(&back, "scale"), 2.5);

    // The file it rewrites is this build's schema, and the v1 file was not
    // condemned. (v1 -> v3 -> v5 in one start, since card 151.)
    studio.stop().await;
    let text = std::fs::read_to_string(dir.0.join("state.json")).expect("a state file");
    assert!(!dir.0.join("state.bad.json").exists());
    let file: serde_json::Value = serde_json::from_str(&text).expect("it parses");
    assert_eq!(file["version"], screeny_studio::state::SCHEMA_VERSION);
    assert_eq!(file["patches"]["plasma"]["params"]["scale"], 2.5, "{text}");
}
