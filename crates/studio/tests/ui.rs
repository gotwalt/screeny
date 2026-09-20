//! The page's wiring, checked without a browser.
//!
//! There is no Node toolchain in this crate and no build step, which rules out
//! every usual way of checking a front end (card 121 is the card for closing
//! that properly). What *can* be checked cheaply, and what actually breaks in
//! practice, is the wiring: every element the script reaches for exists in the
//! page, and every route it calls exists on the server. Both of those are
//! silent failures in a browser and loud ones here.
//!
//! Card 170 folded the dashboard into the one page, so there is one set of
//! files to check instead of two - and `/dashboard` has to keep working as a
//! bookmark.

mod common;

use common::{get, post, studio};

const INDEX_HTML: &str = include_str!("../ui/index.html");
const MAIN_JS: &str = include_str!("../ui/main.js");
const STYLE_CSS: &str = include_str!("../ui/style.css");

/// Every `$('#id')` and `$('.class', ...)` in the script.
fn selectors(js: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = js;
    while let Some(at) = rest.find("$('") {
        rest = &rest[at + 3..];
        if let Some(end) = rest.find('\'') {
            let sel = &rest[..end];
            if sel.starts_with('#') || sel.starts_with('.') {
                out.push(sel.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Every route the script calls: `invoke('path'` and its one wrapper,
/// `call('path'`.
fn routes(js: &str) -> Vec<String> {
    let mut out = Vec::new();
    for opener in ["invoke('", "call('"] {
        let mut rest = js;
        while let Some(at) = rest.find(opener) {
            rest = &rest[at + opener.len()..];
            if let Some(end) = rest.find('\'') {
                out.push(rest[..end].to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_page_is_served_and_so_are_its_two_files() {
    let studio = studio().await;
    let at = studio.addr;

    for path in ["/", "/index.html", "/main.js", "/style.css"] {
        let r = get(at, path).await;
        assert_eq!(r.status, 200, "{path}");
        assert!(!r.body.is_empty(), "{path} is empty");
    }

    // Still no directory traversal, and still a 404 rather than the index for
    // a mistyped asset.
    assert_eq!(get(at, "/nope.js").await.status, 404);
    assert_eq!(get(at, "/../Cargo.toml").await.status, 404);
    assert_eq!(get(at, "/sub/dir.js").await.status, 404);
}

/// The dashboard is the same page now. An old bookmark, and the link card 106
/// put in the design view, both still land somewhere - and its own files are
/// gone rather than lying around unreachable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_dashboard_is_folded_in_and_redirects() {
    let studio = studio().await;
    let at = studio.addr;

    for path in ["/dashboard", "/dashboard.html"] {
        let r = get(at, path).await;
        assert_eq!(r.status, 307, "{path} should redirect to the one page");
    }
    for gone in ["/dashboard.js", "/dashboard.css"] {
        assert_eq!(get(at, gone).await.status, 404, "{gone} should not still be served");
    }
    let index = String::from_utf8_lossy(&get(at, "/").await.body).to_string();
    assert!(!index.contains("/dashboard"), "nothing should link to a page that no longer exists");
}

/// Every element the script reaches for exists in the page. A typo here is a
/// silent `null` in a browser.
#[test]
fn every_element_the_page_reaches_for_exists() {
    let mut missing = Vec::new();
    for sel in selectors(MAIN_JS) {
        let found = if let Some(id) = sel.strip_prefix('#') {
            INDEX_HTML.contains(&format!("id=\"{id}\""))
        } else if let Some(class) = sel.strip_prefix('.') {
            INDEX_HTML.contains(&format!("\"{class}\""))
                || INDEX_HTML.contains(&format!("{class} "))
                || INDEX_HTML.contains(&format!(" {class}\""))
        } else {
            true
        };
        if !found {
            missing.push(sel);
        }
    }
    assert!(missing.is_empty(), "the page's script reaches for elements the page does not have: {missing:?}");
}

/// Every route the script calls exists on the server. A 404 here is a control
/// that does nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_route_the_page_calls_exists() {
    let studio = studio().await;
    let at = studio.addr;
    let found = routes(MAIN_JS);
    assert!(found.len() >= 14, "the scan found suspiciously few routes: {found:?}");

    for route in &found {
        // Reads are GET, changes are POST; an empty object is a valid body for
        // every one of them, and what comes back does not matter here - only
        // that the route is there at all.
        let r = get(at, &format!("/api/v1/{route}")).await;
        let r = if r.status == 405 { post(at, &format!("/api/v1/{route}"), "{}").await } else { r };
        assert_ne!(r.status, 404, "the page calls /api/v1/{route}, which the server does not have");
    }
    println!("routes checked: {}", found.join(", "));
}

/// The page keeps the promises card 106 made for the dashboard, now that it is
/// the only page: a brightness that lights nothing is never offered, and
/// nothing reaches outside the box.
#[test]
fn the_page_keeps_its_promises() {
    assert!(
        MAIN_JS.contains("BRIGHTNESS_FLOOR = 6"),
        "the slider's lowest non-zero stop should be the first value that lights the panel"
    );
    // Card 136 will delete this; it should be one constant and one helper, not
    // a rule sprinkled through the file.
    assert_eq!(MAIN_JS.matches("BRIGHTNESS_FLOOR").count(), 2, "keep the 1..=5 workaround in one place");

    // No CDN, no web font from the network, no module import from anywhere but
    // here: this runs on a LAN box with no promise of internet.
    for bad in ["http://", "https://", "cdn.", "unpkg", "jsdelivr", "fonts.googleapis"] {
        assert!(!MAIN_JS.contains(bad), "the page must not reach outside the box: {bad}");
        assert!(!INDEX_HTML.contains(bad), "the page must not reach outside the box: {bad}");
        assert!(!STYLE_CSS.contains(bad), "the stylesheet must not reach outside the box: {bad}");
    }
}

/// Card 173: the page has somewhere to say whether it is even looking for
/// panels, and the script tells the three cases apart rather than leaving an
/// empty list to mean all of them.
#[test]
fn the_page_can_say_whether_it_is_looking_for_panels() {
    assert!(INDEX_HTML.contains("id=\"discovery-note\""), "the panel section needs a line for the discovery state");
    for case in ["d.enabled", "d.last_error", "d.browses"] {
        assert!(MAIN_JS.contains(case), "the discovery line must distinguish {case}");
    }
    // A browse that finds nothing is normal, so this line is never drawn in
    // the fault tone and never reaches `/healthz`.
    let line = MAIN_JS.find("function discoveryLine").expect("the discovery line");
    let body = &MAIN_JS[line..line + 1200];
    assert!(!body.contains("'bad'"), "a browse that finds nothing is not a fault");
}

/// Card 145: the GPU outcome is part of the studio's state rather than a line
/// on stderr. `bootstrap` says which pieces need an adapter and whether there
/// is one; `/api/v1/status` says the same thing; and a missing adapter is
/// never a 503.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gpu_outcome_is_on_the_api_and_is_never_a_fault() {
    let studio = studio().await;
    let at = studio.addr;

    let boot = get(at, "/api/v1/bootstrap").await.json();
    let gpu = &boot["gpu"];
    assert!(gpu["available"].is_boolean(), "bootstrap should carry the adapter outcome: {boot}");
    let pieces = boot["pieces"].as_array().expect("a list of pieces");
    assert!(pieces.iter().all(|p| p["needs_gpu"].is_boolean()), "every piece says whether it needs an adapter");
    // Built with the `gpu` feature, so there are some; without it there are
    // none, and that is the truth for that build.
    let marked: Vec<&str> = pieces
        .iter()
        .filter(|p| p["needs_gpu"] == true)
        .filter_map(|p| p["id"].as_str())
        .collect();
    assert_eq!(marked, screeny_art::pieces::NEEDS_GPU.to_vec(), "the marked pieces are exactly the GPU ones");

    let status = get(at, "/api/v1/status").await.json();
    assert_eq!(status["gpu"], *gpu, "the two routes must not be able to disagree");
    assert_eq!(status["ok"], true, "a missing adapter is not a server fault");
    assert_eq!(get(at, "/healthz").await.status, 200);

    // Whichever way this machine answered, one of the two halves is filled in.
    if gpu["available"] == true {
        assert!(!gpu["adapter"].as_str().unwrap_or("").is_empty(), "an available adapter has a name: {gpu}");
        assert_eq!(gpu["error"], serde_json::Value::Null);
    } else {
        assert!(!gpu["error"].as_str().unwrap_or("").is_empty(), "an unavailable adapter has a reason: {gpu}");
    }
}

/// And the page draws it: the GPU pieces are marked unavailable rather than
/// offered and then black, and the reason is on the page.
#[test]
fn the_page_says_why_a_gpu_piece_is_not_available() {
    assert!(INDEX_HTML.contains("id=\"gpu-note\""), "the piece list needs a line for the adapter");
    assert!(MAIN_JS.contains("input.disabled = true"), "a piece that cannot draw must not be offered");
    assert!(MAIN_JS.contains("needs_gpu"), "the page reads the per-piece flag from bootstrap");
    assert!(STYLE_CSS.contains("data-unavailable"), "an unavailable piece has to look unavailable");
}

/// The same fact over the API, which is what the line is drawn from: with
/// discovery off, `/api/v1/status` says so rather than looking like a browse
/// that has found nothing yet.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_says_whether_discovery_is_on() {
    let studio = studio().await; // `test_config`: discovery off, as `--no-discover`
    let status = get(studio.addr, "/api/v1/status").await.json();
    assert_eq!(status["discovery"]["enabled"], false, "{}", status["discovery"]);
    assert_eq!(status["discovery"]["browses"], 0);
    assert_eq!(status["discovery"]["last_error"], serde_json::Value::Null, "not looking is not an error");
    assert_eq!(status["ok"], true, "not looking for panels is never unhealthy");
}

/// Card 172: the rate control can show any rate a player may be on, and its
/// range is the player's range rather than a second opinion about it.
#[test]
fn the_rate_control_spans_the_players_whole_range() {
    let span = format!(
        r#"id="fps" type="range" min="{}" max="{}" step="1""#,
        screeny_studio::player::MIN_FPS as u32,
        screeny_studio::player::MAX_FPS as u32
    );
    assert!(INDEX_HTML.contains(&span), "the rate slider must span MIN_FPS..=MAX_FPS; looked for {span}");
    assert!(!INDEX_HTML.contains(r#"name="fps""#), "the two-stop radio group is gone");
}

/// And the rate a script set is the rate the page reports - it is not quietly
/// changed by a control that could not express it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rate_set_through_the_api_is_what_the_page_reports() {
    let studio = studio().await;
    let at = studio.addr;

    // A panel to aim a player at. Nothing is sent: it is never switched on.
    let add = post(at, "/api/v1/devices/add", r#"{"to":"127.0.0.1:50999","name":"paper panel"}"#).await;
    assert_eq!(add.status, 200, "{}", String::from_utf8_lossy(&add.body));
    let device = add.json()["id"].as_str().expect("an id").to_string();

    // The page is a window onto this player, so `player/set` and the page's
    // own state are the same fps.
    let set = post(at, "/api/v1/set_panel", &format!(r#"{{"on":true,"to":"{device}"}}"#)).await;
    assert_eq!(set.status, 200, "{}", String::from_utf8_lossy(&set.body));

    for rate in [10.0, 15.0, 24.0, 45.0] {
        let body = format!(r#"{{"device":"{device}","fps":{rate}}}"#);
        let player = post(at, "/api/v1/player/set", &body).await;
        assert_eq!(player.status, 200, "{}", String::from_utf8_lossy(&player.body));
        assert_eq!(player.json()["fps"], rate);
        // What a browser reloading would draw its control from.
        let boot = get(at, "/api/v1/bootstrap").await.json();
        assert_eq!(boot["state"]["fps"], rate, "the page has to be able to show {rate} fps");
    }

    // And a rate that is not a number does not reach the render loop, where
    // `Duration::from_secs_f64(NaN)` would panic.
    let body = format!(r#"{{"device":"{device}","fps":1e400}}"#);
    let player = post(at, "/api/v1/player/set", &body).await;
    let still = get(at, "/api/v1/bootstrap").await.json();
    assert_eq!(still["state"]["fps"], 45.0, "an infinite rate left it where it was: {}", player.status);
    studio.stop().await;
}

/// Card 170's layout requirement, as far as a text file can carry it: the
/// two-column bench is behind a breakpoint, so at every narrower width the
/// page is an ordinary scrolling column and the picture cannot overlap the
/// controls. The four widths are checked in a real browser and the
/// screenshots are in the card's Log; this is the guard that stops the rule
/// being deleted by accident.
#[test]
fn the_two_column_layout_is_behind_a_breakpoint() {
    assert!(
        STYLE_CSS.contains("@media (min-width: 1100px)"),
        "the bench layout must be opt-in at wide widths, not the default"
    );
    let bench = STYLE_CSS.find("@media (min-width: 1100px)").expect("the breakpoint");
    let before = &STYLE_CSS[..bench];
    assert!(
        !before.contains("grid-template-areas"),
        "the narrow layout must not be a grid with a fixed side column - that is the ~600 px overlap the owner saw"
    );
}

/// The sections stack in the order the card asks for: picture, now playing,
/// parameters, panel. That is DOM order, so it is what a narrow window gets
/// with no CSS help at all.
#[test]
fn the_page_is_in_the_order_it_should_stack_in() {
    let at = |needle: &str| INDEX_HTML.find(needle).unwrap_or_else(|| panic!("`{needle}` is in the page"));
    let order = [
        ("the picture", at("id=\"stage\"")),
        ("now playing", at("id=\"sec-now\"")),
        ("parameters", at("id=\"sec-params\"")),
        ("the panel", at("id=\"sec-panel\"")),
    ];
    for pair in order.windows(2) {
        assert!(pair[0].1 < pair[1].1, "{} should come before {}", pair[0].0, pair[1].0);
    }
    // And the meters, which are a detail, come after all of them.
    assert!(at("class=\"meters\"") > order[3].1, "the meters belong at the bottom of a narrow page");
}
