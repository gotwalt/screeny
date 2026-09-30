//! The page's wiring, checked without a browser.
//!
//! There is no Node toolchain in this crate and no build step, which rules out
//! every usual way of checking a front end (card 121 is the card for closing
//! that properly). What *can* be checked cheaply, and what actually breaks in
//! practice, is the wiring: every element the script reaches for exists in the
//! page, and every route it calls exists on the server. Both of those are
//! silent failures in a browser and loud ones here.
//!
//! Card 126 is the one exception: `common.js`'s brightness hold logic is a
//! small pure function, and pinning it directly is worth *running* `node` at
//! test time when the machine happens to have it - still no build step, no
//! dependency, and the test skips cleanly (saying why) when there is none.
//!
//! Card 170 folded the dashboard into the one page; card 198 split that page
//! into two **screens** - the Picture at `/` and the Panel at `/panel` - and
//! card 301 added a third, tied to the other two by one nav: the Schedule at
//! `/schedule` until card 310 retired the timetable, and since card 311 the
//! Settings at `/settings`. `/dashboard` and `/schedule` still have to work as
//! bookmarks.
//!
//! So the wiring check is now per screen: every element `picture.js` reaches
//! for is in `index.html`, every element `panel.js` reaches for is in
//! `panel.html`, every element `settings.js` reaches for is in
//! `settings.html`, and every element `common.js` reaches for is in **all
//! three** - which is the rule that keeps the shared file shared.

mod common;

use common::{get, post, studio};
use std::path::Path;
use std::process::Command;

const INDEX_HTML: &str = include_str!("../ui/index.html");
const PANEL_HTML: &str = include_str!("../ui/panel.html");
const SETTINGS_HTML: &str = include_str!("../ui/settings.html");
const COMMON_JS: &str = include_str!("../ui/common.js");
const PICTURE_JS: &str = include_str!("../ui/picture.js");
const PANEL_JS: &str = include_str!("../ui/panel.js");
const SETTINGS_JS: &str = include_str!("../ui/settings.js");
const STYLE_CSS: &str = include_str!("../ui/style.css");

/// The whole front end, for the claims that are about it rather than about one
/// screen.
fn all_js() -> String {
    format!("{COMMON_JS}\n{PICTURE_JS}\n{PANEL_JS}\n{SETTINGS_JS}")
}

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
async fn all_three_screens_are_served_and_so_are_their_files() {
    let studio = studio().await;
    let at = studio.addr;

    // (The content types are `ui::content_type`'s and are checked over real
    // HTTP in the card's Log; the test client keeps only the body.)
    for path in [
        "/", "/index.html", "/panel", "/panel/", "/panel.html", "/settings", "/settings/", "/settings.html",
        "/common.js", "/picture.js", "/panel.js", "/settings.js", "/style.css",
    ] {
        let r = get(at, path).await;
        assert_eq!(r.status, 200, "{path}");
        assert!(!r.body.is_empty(), "{path} is empty");
    }

    // `/panel` is a screen and `/panel.js` is a file: the tidy URL must not
    // swallow the script that happens to share its name. Same for `/settings`.
    let screen = String::from_utf8_lossy(&get(at, "/panel").await.body).to_string();
    let script = String::from_utf8_lossy(&get(at, "/panel.js").await.body).to_string();
    assert!(screen.starts_with("<!doctype html>"), "/panel should be the screen");
    assert!(script.starts_with("// The Studio's Panel screen"), "/panel.js should be the script");
    let settings_screen = String::from_utf8_lossy(&get(at, "/settings").await.body).to_string();
    let settings_script = String::from_utf8_lossy(&get(at, "/settings.js").await.body).to_string();
    assert!(settings_screen.starts_with("<!doctype html>"), "/settings should be the screen");
    assert!(settings_script.starts_with("// The Studio's Settings screen"), "/settings.js should be the script");

    // Each screen loads its own module, and all three load the shared one
    // through it rather than with a second <script> tag.
    assert!(screen.contains(r#"src="/panel.js""#), "the panel screen loads its own script");
    assert!(PANEL_JS.contains("from './common.js'"), "...which imports the shared one");
    assert!(settings_screen.contains(r#"src="/settings.js""#), "the settings screen loads its own script");
    assert!(SETTINGS_JS.contains("from './common.js'"), "...which imports the shared one too");

    // Still no directory traversal, and still a 404 rather than the index for
    // a mistyped asset.
    assert_eq!(get(at, "/nope.js").await.status, 404);
    assert_eq!(get(at, "/main.js").await.status, 404, "the one page's script is gone, not renamed in place");
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

    // Card 310: the Schedule screen went with the timetable; its address
    // lands on the Picture screen rather than on a 404.
    for path in ["/dashboard", "/dashboard.html", "/schedule", "/schedule.html"] {
        let r = get(at, path).await;
        assert_eq!(r.status, 307, "{path} should redirect to the one page");
    }
    assert_eq!(get(at, "/schedule.js").await.status, 404, "the Schedule screen's script is gone");
    for gone in ["/dashboard.js", "/dashboard.css"] {
        assert_eq!(get(at, gone).await.status, 404, "{gone} should not still be served");
    }
    let index = String::from_utf8_lossy(&get(at, "/").await.body).to_string();
    assert!(!index.contains("/dashboard"), "nothing should link to a page that no longer exists");
}

/// Is `sel` in this page?
fn has(html: &str, sel: &str) -> bool {
    if let Some(id) = sel.strip_prefix('#') {
        html.contains(&format!("id=\"{id}\""))
    } else if let Some(class) = sel.strip_prefix('.') {
        html.contains(&format!("\"{class}\"")) || html.contains(&format!("{class} ")) || html.contains(&format!(" {class}\""))
    } else {
        true
    }
}

/// Every element a screen's script reaches for exists in that screen. A typo
/// here is a silent `null` in a browser.
///
/// Card 198: three scripts and two screens, so the check is per pair - and
/// `common.js`, which both screens load, may only reach for what **both** of
/// them have. That is what stops a shared function quietly half-working on the
/// screen that has not got the element.
#[test]
fn every_element_each_screen_reaches_for_exists() {
    let mut missing = Vec::new();
    for (what, js, pages) in [
        ("picture.js", PICTURE_JS, &[("index.html", INDEX_HTML)][..]),
        ("panel.js", PANEL_JS, &[("panel.html", PANEL_HTML)][..]),
        ("settings.js", SETTINGS_JS, &[("settings.html", SETTINGS_HTML)][..]),
        (
            "common.js",
            COMMON_JS,
            &[("index.html", INDEX_HTML), ("panel.html", PANEL_HTML), ("settings.html", SETTINGS_HTML)][..],
        ),
    ] {
        for sel in selectors(js) {
            for (page, html) in pages {
                if !has(html, &sel) {
                    missing.push(format!("{what} reaches for {sel}, which {page} has not got"));
                }
            }
        }
    }
    assert!(missing.is_empty(), "{missing:#?}");
}

/// Card 198's acceptance, as far as a text file can carry it: **nothing about
/// devices is on the Picture screen**, and everything that was in the old
/// Panel section is on the Panel screen. Card 301 tightened it further:
/// brightness, the panel model and the limiter moved there too, so the only
/// thing the Picture screen still carries about the panel is the status chip.
#[test]
fn the_screens_hold_what_the_split_says_they_do() {
    // The panel's own affairs, by the ids they are drawn into. Every one of
    // them was on the one page before this card.
    for gone in [
        "discovery-note", "panel-out", "panel-out-label", "panel-facts", "device-block",
        "device-facts", "device-note", "identify", "rename", "reboot", "setup", "add-to",
    ] {
        assert!(!INDEX_HTML.contains(&format!("id=\"{gone}\"")), "#{gone} belongs on the Panel screen");
        assert!(PANEL_HTML.contains(&format!("id=\"{gone}\"")), "#{gone} has to still exist somewhere");
    }
    // Nor is any of it *said* on the Picture screen.
    for word in ["WiFi", "Reboot", "heap", "discovery", "mDNS"] {
        assert!(!INDEX_HTML.contains(word), "the Picture screen should not talk about {word}");
    }
    // Card 301: brightness moved to the Panel screen entirely - it is a panel
    // setting, not a way of judging a patch - so it is gone from the Picture
    // screen along with the panel model and the limiter. `bindBrightness`
    // stays a shared function in `common.js` even with one caller now, so a
    // second one cannot drift from it if the day comes back that there is one.
    for gone in ["bright", "bright-slider", "bright-note", "panel-kind", "dither", "panel-model", "codec-preview", "limiter-on", "apl-slider", "rise-slider"] {
        assert!(!INDEX_HTML.contains(&format!("id=\"{gone}\"")), "#{gone} belongs on the Panel screen now");
        assert!(PANEL_HTML.contains(&format!("id=\"{gone}\"")), "#{gone} has to still exist somewhere");
    }
    assert!(COMMON_JS.contains("export function bindBrightness"), "the shared function stays, even with one caller");
    assert!(
        INDEX_HTML.contains(r#"<a class="pill pill--link" id="ro-panel" href="/panel">"#),
        "the status chip is the way to the Panel screen"
    );
    // The way back is the shared nav now, not a one-off backlink - checked in
    // full by `the_nav_is_on_every_screen_with_the_current_one_marked`.
    assert!(!PANEL_HTML.contains("backlink"), "the nav replaced the one-off backlink");

    // The picture is not on the Panel screen at all - no canvas, and so no
    // frames asked for (card 120's rule, restated for a screen that draws
    // none).
    assert!(!PANEL_HTML.contains("<canvas"), "the Panel screen draws no picture");
    assert!(PANEL_JS.contains("}, noFrames, { panel: chosen });"), "so it must ask the socket for none");
    assert!(COMMON_JS.contains("export const noFrames = () => 0;"), "fps 0 is what asks for none");
    assert!(!PANEL_JS.contains("requestAnimationFrame"), "and it has no frame pump");
}

/// Every route the script calls exists on the server. A 404 here is a control
/// that does nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_route_the_page_calls_exists() {
    let studio = studio().await;
    let at = studio.addr;
    let found = routes(&all_js());
    // Card 301 removed three calls the page made (`set_seed`, `set_playback`,
    // `restart`) without replacing them, so the floor comes down with it - this
    // is still a smoke check that the scanner itself is finding real routes,
    // not a pin on the exact count.
    assert!(found.len() >= 11, "the scan found suspiciously few routes: {found:?}");

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

/// Both screens keep the promises card 106 made for the dashboard: a
/// brightness that lights nothing is never offered, and nothing reaches
/// outside the box.
#[test]
fn the_screens_keep_their_promises() {
    // Card 187: the floor used to be a literal `6` copied into `common.js`
    // (card 136's note said as much: "delete this ... when 136 fixes the
    // firmware"). It is gone from the front end entirely now - the server
    // computes the real stops (`page::brightness_stops`, one implementation
    // of the output-enable slot arithmetic) and the page only ever indexes
    // into what it was handed.
    for (what, text) in [("common.js", COMMON_JS), ("picture.js", PICTURE_JS), ("panel.js", PANEL_JS)] {
        assert!(!text.contains("BRIGHTNESS_FLOOR"), "{what} should not know the floor's value itself");
    }
    assert!(COMMON_JS.contains("function cappedStops("), "the cap still trims the server's stops, once, here");
    // Card 301: brightness moved off the Picture screen entirely, so the Panel
    // screen is the one caller of the shared control now - not both.
    assert!(PANEL_JS.contains("stops: boot.brightness_stops"), "panel.js should hand the control the server's stops");
    assert!(!PICTURE_JS.contains("boot.brightness_stops"), "brightness is not on the Picture screen any more");

    // No CDN, no web font from the network, no module import from anywhere but
    // here: this runs on a LAN box with no promise of internet.
    for bad in ["http://", "https://", "cdn.", "unpkg", "jsdelivr", "fonts.googleapis"] {
        for (what, text) in [
            ("index.html", INDEX_HTML),
            ("panel.html", PANEL_HTML),
            ("settings.html", SETTINGS_HTML),
            ("common.js", COMMON_JS),
            ("picture.js", PICTURE_JS),
            ("panel.js", PANEL_JS),
            ("settings.js", SETTINGS_JS),
            ("style.css", STYLE_CSS),
        ] {
            assert!(!text.contains(bad), "{what} must not reach outside the box: {bad}");
        }
    }
    // And no build step: the only thing either script imports is a file in
    // this directory.
    for import in all_js().split("from '").skip(1) {
        let from = import.split('\'').next().unwrap_or_default();
        assert_eq!(from, "./common.js", "the front end imports nothing but the shared module");
    }
}

/// Card 187's shape: `GET /api/v1/bootstrap` really does carry the stops, not
/// just `page::brightness_stops` in isolation - the wiring, not the formula
/// (that is `page::tests::there_is_one_stop_per_real_picture`, next to the
/// function itself).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_carries_the_brightness_stops() {
    let studio = studio().await;
    let at = studio.addr;
    let boot = get(at, "/api/v1/bootstrap").await.json();
    let stops: Vec<u64> = boot["brightness_stops"].as_array().expect("a list").iter().map(|v| v.as_u64().unwrap()).collect();
    assert_eq!(stops.first(), Some(&0), "off is always the first stop: {stops:?}");
    assert!(stops.windows(2).all(|w| w[0] < w[1]), "strictly increasing: {stops:?}");
    assert!(stops.len() > 2 && stops.len() < 256, "the real resolution, not 256 raw values: {stops:?}");
}

/// Card 126: the slider bounced after a brightness change because `show()`
/// always preferred the panel's periodic telemetry over
/// `player.health.brightness_applied`, even in the second or so right after a
/// change when telemetry still carried the old number (`health.brightness_applied`
/// is written the instant the change's own POST resolves - measured against
/// `screeny-sim` in the card's Log). `brightnessHoldWins` is the rule that
/// fixes it, pulled out as a pure function so it can be pinned exactly.
///
/// Runs the real, shipped `common.js` under `node`, not a copy - so a change
/// to the function is what this test sees. Skipped, loudly, when there is no
/// `node` on `PATH`: this crate has no Node dependency at build time, and a
/// worker or a bench without one still gets a clean `cargo test`.
#[test]
fn brightness_hold_wins_pins_the_hold_and_release_rules() {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!(
            "skipping brightness_hold_wins_pins_the_hold_and_release_rules: no `node` on PATH \
             (this crate has no Node dependency at build time; the test just cannot run here)"
        );
        return;
    }

    let common_js = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/common.js");
    let script = format!(
        r#"
import {{ brightnessHoldWins as wins }} from 'file://{path}';
const cases = [
  // [now, until, held, reading, expected, label]
  [0,    1000, 80, null, true,  "nothing to compare against yet: keep holding"],
  [0,    1000, 80, 30,   true,  "reading still disagrees, well before the deadline"],
  [999,  1000, 80, 30,   true,  "reading still disagrees, one ms before the deadline"],
  [0,    1000, 80, 80,   false, "the reading agrees: release at once, not at the deadline"],
  [1000, 1000, 80, 30,   false, "the deadline has passed and the reading never agreed: let it through"],
  [5000, 1000, 80, 30,   false, "long past the deadline: still released"],
];
let failed = 0;
for (const [now, until, held, reading, expected, label] of cases) {{
  const got = wins(now, until, held, reading);
  if (got !== expected) {{
    failed += 1;
    console.error(`FAIL ${{label}}: wins(${{now}}, ${{until}}, ${{held}}, ${{reading}}) = ${{got}}, want ${{expected}}`);
  }}
}}
if (failed) {{ process.exit(1); }}
console.log(`${{cases.length}} cases passed`);
"#,
        path = common_js.display(),
    );

    let output = Command::new("node")
        .args(["--input-type=module", "-e", &script])
        .output()
        .expect("node is on PATH; just checked above");
    assert!(
        output.status.success(),
        "brightnessHoldWins:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Card 312: the Panel screen's brightness reads in percent of full light,
/// the unit Home Assistant's slider uses (card 310) - and it must be **the same
/// number**, level for level. Runs the shipped `percentOf` under `node` over
/// every level 0-255 against the server's own stops, and compares each with
/// `ha::payload::percent_of`, the one HA is sent. Skipped, loudly, without
/// `node`, like the test above.
#[test]
fn the_panel_screen_and_home_assistant_say_the_same_percent() {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipping the_panel_screen_and_home_assistant_say_the_same_percent: no `node` on PATH");
        return;
    }
    let stops = screeny_studio::page::brightness_stops();
    let want: Vec<u8> = (0..=u8::MAX).map(screeny_studio::ha::payload::percent_of).collect();
    let common_js = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/common.js");
    let script = format!(
        r#"
import {{ percentOf }} from 'file://{path}';
const stops = {stops:?};
console.log(JSON.stringify(Array.from({{ length: 256 }}, (_, l) => percentOf(l, stops))));
"#,
        path = common_js.display(),
    );
    let output = Command::new("node").args(["--input-type=module", "-e", &script]).output().expect("node is on PATH");
    assert!(output.status.success(), "percentOf: {}", String::from_utf8_lossy(&output.stderr));
    let got: Vec<u8> = serde_json::from_slice(&output.stdout).expect("a JSON list of percents");
    assert_eq!(got, want, "the page and Home Assistant must say the same percent for every level");
    assert_eq!((got[0], got[stops[1] as usize], got[255]), (0, 4, 100), "dark, the dimmest visible step, full");
}

/// Card 164: **what a panel costs the network is on the Panel screen, and the
/// page does not do the arithmetic.**
///
/// The rate is worked out once, on the server, on the supervisor's own tick -
/// that is what makes two browsers agree - so what has to be pinned here is
/// that the page *reads* it. A `/` in this block would be a second opinion, in
/// the same way a threshold written into the page would be (card 195).
#[test]
fn the_network_line_is_on_the_panel_screen_and_is_read_rather_than_worked_out() {
    assert!(PANEL_JS.contains("function networkRows("), "the Panel screen draws the network line");
    assert!(!PICTURE_JS.contains("traffic"), "nothing about network traffic on the Picture screen");

    // It reads the server's own figures rather than deriving any of them.
    for field in ["r.out", "r.in", "r.frames_out", "r.control_out", "r.http_out", "t.total.out.bytes"] {
        assert!(PANEL_JS.contains(field), "the network line should read `{field}` from the server");
    }
    let block = PANEL_JS
        .split("function networkRows(")
        .nth(1)
        .and_then(|s| s.split("\n  }").next())
        .expect("the network block");
    assert!(!block.contains(" / "), "the page must not compute a rate of its own: {block}");

    // KB is 1000 bytes here, which is not what `kb`/`size` mean - those are
    // about memory. Two functions, deliberately, and the shared file says why.
    assert!(COMMON_JS.contains("export function kbs("), "a network rate has its own formatter");
    assert!(COMMON_JS.contains("export function netSize("), "and so does a network total");
    assert!(COMMON_JS.contains("/ 1000"), "network numbers are in powers of ten");
}

/// Card 173: the page has somewhere to say whether it is even looking for
/// panels, and the script tells the three cases apart rather than leaving an
/// empty list to mean all of them.
#[test]
fn the_page_can_say_whether_it_is_looking_for_panels() {
    assert!(PANEL_HTML.contains("id=\"discovery-note\""), "the panel section needs a line for the discovery state");
    for case in ["d.enabled", "d.last_error", "d.browses"] {
        assert!(PANEL_JS.contains(case), "the discovery line must distinguish {case}");
    }
    // A browse that finds nothing is normal, so this line is never drawn in
    // the fault tone and never reaches `/healthz`.
    let line = PANEL_JS.find("function discoveryLine").expect("the discovery line");
    let body = &PANEL_JS[line..line + 1200];
    assert!(!body.contains("'bad'"), "a browse that finds nothing is not a fault");
}

/// Card 145: the GPU outcome is part of the studio's state rather than a line
/// on stderr. `bootstrap` says which patches need an adapter and whether there
/// is one; `/api/v1/status` says the same thing; and a missing adapter is
/// never a 503.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gpu_outcome_is_on_the_api_and_is_never_a_fault() {
    let studio = studio().await;
    let at = studio.addr;

    let boot = get(at, "/api/v1/bootstrap").await.json();
    let gpu = &boot["gpu"];
    assert!(gpu["available"].is_boolean(), "bootstrap should carry the adapter outcome: {boot}");
    let patches = boot["patches"].as_array().expect("a list of patches");
    assert!(patches.iter().all(|p| p["needs_gpu"].is_boolean()), "every patch says whether it needs an adapter");
    // Built with the `gpu` feature, so there are some; without it there are
    // none, and that is the truth for that build.
    let marked: Vec<&str> = patches
        .iter()
        .filter(|p| p["needs_gpu"] == true)
        .filter_map(|p| p["id"].as_str())
        .collect();
    assert_eq!(marked, screeny_art::patches::NEEDS_GPU.to_vec(), "the marked patches are exactly the GPU ones");

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

/// And the page draws it: the GPU patches are marked unavailable rather than
/// offered and then black, and the reason is on the page.
#[test]
fn the_page_says_why_a_gpu_patch_is_not_available() {
    assert!(INDEX_HTML.contains("id=\"gpu-note\""), "the patch list needs a line for the adapter");
    assert!(PICTURE_JS.contains("input.disabled = true"), "a patch that cannot draw must not be offered");
    assert!(PICTURE_JS.contains("needs_gpu"), "the page reads the per-patch flag from bootstrap");
    assert!(STYLE_CSS.contains("data-unavailable"), "an unavailable patch has to look unavailable");
}

/// Card 180: the page has somewhere to put what only the device knows, it is
/// hidden when there is nothing to put there, and it never invents a threshold
/// of its own - the server decides what stands out, in one place, beside the
/// reasoning.
#[test]
fn the_page_can_show_what_only_the_device_knows() {
    assert!(PANEL_HTML.contains("id=\"device-block\""), "the panel section needs a block for the device's own facts");
    assert!(PANEL_HTML.contains("id=\"device-facts\""), "...and a list inside it");
    assert!(PANEL_HTML.contains("id=\"device-block\" hidden"), "with no HTTP status API the page is the page it was");
    assert!(STYLE_CSS.contains(".device-block"), "the block has to look like part of the panel section");

    // Every flag the block draws a tone from is the server's judgement, read
    // by name. A number here would be a second opinion about a threshold.
    // Card 195: two levels for the stack, and the reboot nobody asked for.
    // Card 199: `p.repeat` (`last_panic.consecutive > 1`) is the panic
    // breadcrumb's own one.
    for decided in [
        "f.stack_warn",
        "f.stack_fault",
        "f.low_heap",
        "f.bad_fw_state",
        "f.odd_reset",
        "f.store_errors",
        "f.unasked_reboots",
        "p.repeat",
    ] {
        assert!(PANEL_JS.contains(decided), "the page reads {decided} rather than deciding it");
    }
    let body = device_block();
    // Every threshold that has ever been one, including the ones card 195
    // replaced: none of them belongs in a browser.
    for invented in ["2048", "4096", "8192", "0.85", "< 4312", "98304"] {
        assert!(!body.contains(invented), "the page must not carry its own copy of a threshold: {invented}");
    }
    // The six that are meant to be loud are loud, and nothing else is: a
    // reset that should not have happened, a store error, a stack past the
    // fault line and a heap past it (card 195 made both of those faults), and
    // - card 199 - repeated panics and a watchdog reset.
    assert_eq!(body.matches("'bad'").count(), 6, "the six fault tones, and only those: {body}");
    // ...and the reboot the studio did not ask for is *not* one of them.
    let reboots = body.find("['Reboots'").expect("the reboots row");
    let row = &body[reboots..body[reboots..].find('\n').map_or(body.len(), |n| reboots + n)];
    assert!(!row.contains("'bad'") && !row.contains("'warn'"), "an unasked-for reboot is said quietly: {row}");
}

/// `showDevice` and its helpers, from its doc comment to the next one. Sliced
/// by hand because "the page carries no threshold of its own" is a claim about
/// this block and not about the whole file - the front end is full of numbers
/// that are layout.
fn device_block() -> &'static str {
    let start = PANEL_JS.find("function showDevice").expect("the device block");
    let rest = &PANEL_JS[start..];
    // The next top-level doc comment after `rebootLine`, which is the last
    // helper this block owns.
    let after = rest.find("function rebootLine").expect("the reboots row helper");
    let end = rest[after..].find("\n  /**").map_or(rest.len(), |n| after + n);
    &rest[..end]
}

/// And the studio's `/api/v1/status` carries the thresholds' verdicts rather
/// than only the raw numbers, so the two cannot drift apart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_with_no_http_status_api_leaves_the_page_as_it_was() {
    let studio = studio().await;
    let at = studio.addr;
    // A panel that will never answer anything: nothing is on that port.
    let add = post(at, "/api/v1/devices/add", r#"{"to":"127.0.0.1:50998","name":"paper panel"}"#).await;
    assert_eq!(add.status, 200, "{}", String::from_utf8_lossy(&add.body));

    let status = get(at, "/api/v1/status").await.json();
    let d = &status["devices"][0];
    assert!(d["facts"].is_null(), "nothing has been read: {d}");
    assert_eq!(d["http"]["reads"], 0, "{}", d["http"]);
    assert_eq!(status["ok"], true, "a panel that has never answered is not a server fault: {}", status["problems"]);
    assert_eq!(get(at, "/healthz").await.status, 200);
    studio.stop().await;
}

/// Card 181: with no panel attached, no control in the panel section claims
/// something is reaching a panel.
///
/// The switch stays live, because it is not decorative: `state.on` is what
/// makes the first panel found start playing without anybody pressing
/// anything, and disabling it would take that choice away. What changes is
/// what it says it does - which is card 170's standard, that no control's
/// effect on the panel is unclear.
#[test]
fn the_output_switch_says_what_it_does_when_there_is_no_panel() {
    assert!(PANEL_HTML.contains("id=\"panel-out-label\""), "the switch's label has to be writable");
    assert!(
        PANEL_JS.contains("'Drive a panel as soon as one is found'"),
        "with no panel attached the switch must not promise one"
    );
    assert!(PANEL_JS.contains("'Show it on the panel'"), "and with one attached it says so again");
    // Still live, and still the same two bodies a script drives it with.
    assert!(!PANEL_JS.contains("outSwitch.disabled"), "the switch still decides what the first panel found does");
    // Card 351: both name the panel, since `{"on":false}` alone lets every
    // panel go, and this switch is about the one the screen is on.
    assert!(
        PANEL_JS.contains("{ on: true, to: '', panel } : { on: false, panel }"),
        "`set_panel`'s two bodies, each for this panel"
    );
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

/// Card 161: **no control on either screen offers a frame rate.** The author
/// asked for one rate with no variability, so the rate slider card 172 built
/// (and the two buttons card 105 had before it) are gone, and nothing was left
/// behind that could set one.
#[test]
fn no_control_offers_a_frame_rate() {
    for (what, html) in [("the Picture screen", INDEX_HTML), ("the Panel screen", PANEL_HTML)] {
        assert!(!html.contains(r#"id="fps""#), "{what} still has a rate input");
        assert!(!html.contains(r#"name="fps""#), "{what} still has a rate radio group");
        assert!(!html.contains("fps-stops"), "{what} still declares rate stops");
        assert!(!html.contains(">Frames per second<"), "{what} still labels a rate control");
    }
    for (what, js) in [("picture.js", PICTURE_JS), ("panel.js", PANEL_JS)] {
        assert!(!js.contains("'set_playback', { paused: state.paused, speed: state.speed, fps"), "{what} still sends a rate");
        assert!(!js.contains("$('#fps')"), "{what} still reaches for a rate control");
    }
    // Nor a **readout** of it, since 2026-09-26: the rate has one right answer,
    // and a machine that cannot hold it is a fault for /healthz to raise, not
    // a number for a person to watch. The Time readout (seconds since the
    // studio started, meaningless after weeks) went with it.
    for gone in [r#"id="ro-fps""#, r#"id="ro-time""#, "<dt>Rate</dt>", "<dt>Time</dt>"] {
        assert!(!INDEX_HTML.contains(gone), "the Picture screen still shows {gone}");
    }
    assert!(!PICTURE_JS.contains("$('#ro-fps')"), "picture.js still fills in a rate readout");
    assert!(!PICTURE_JS.contains("$('#ro-time')"), "picture.js still fills in a time readout");
}

/// Card 183 and 197 built a mechanism (`drawStops`, a `<datalist>` drawn on
/// the track) for exactly one slider that ever used it: the rate slider, then
/// Speed after card 161 retired the first. Card 301 retired Speed too - "I
/// don't think speed should be varyable", the owner said - which leaves
/// nothing on either screen that declares a `list=` of stops. Rather than
/// leave a mechanism with no caller, it went with the last control that used
/// it: no datalist, no marks, no `drawStops`.
#[test]
fn no_slider_declares_stops_any_more_and_the_drawing_mechanism_is_gone_with_them() {
    for (what, html) in [("index.html", INDEX_HTML), ("panel.html", PANEL_HTML), ("settings.html", SETTINGS_HTML)] {
        assert!(!html.contains("list=\""), "{what} still declares a slider's stops");
        assert!(!html.contains("<datalist"), "{what} still has a datalist");
    }
    assert!(!COMMON_JS.contains("drawStops"), "the drawing mechanism should go with its last caller");
    assert!(!STYLE_CSS.contains(".stops"), "and so should the marks' own styling");
}

/// And the rate a script set is the rate the page reports - it is not quietly
/// changed by a control that could not express it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rate_sent_by_an_older_client_is_accepted_and_ignored() {
    let studio = studio().await;
    let at = studio.addr;

    // A panel to aim a player at. Nothing is sent: it is never switched on.
    let add = post(at, "/api/v1/devices/add", r#"{"to":"127.0.0.1:50999","name":"paper panel"}"#).await;
    assert_eq!(add.status, 200, "{}", String::from_utf8_lossy(&add.body));
    let device = add.json()["id"].as_str().expect("an id").to_string();

    // The page is a window onto this player, so `player/set` and the page's
    // own state are the same player.
    let set = post(at, "/api/v1/set_panel", &format!(r#"{{"on":true,"to":"{device}"}}"#)).await;
    assert_eq!(set.status, 200, "{}", String::from_utf8_lossy(&set.body));

    // Every rate card 172's slider could reach, plus the ones that used to
    // need special handling: a body that still carries `fps` is taken in full
    // and the field does nothing.
    for rate in ["10", "15", "24", "45", "60", "0", "1e400", "null"] {
        let body = format!(r#"{{"device":"{device}","seed":7,"fps":{rate}}}"#);
        let player = post(at, "/api/v1/player/set", &body).await;
        assert_eq!(player.status, 200, "fps {rate}: {}", String::from_utf8_lossy(&player.body));
        assert_eq!(player.json()["seed"], 7, "fps {rate}: the rest of the body was applied");
        assert_eq!(player.json()["fps"], 30.0, "fps {rate}: the one rate is what comes back");
        // What a browser reloading would draw itself from.
        let boot = get(at, "/api/v1/bootstrap").await.json();
        assert_eq!(boot["state"]["fps"], 30.0, "fps {rate}: and the page is told the one rate");
    }

    // The page's own route, with card 172's body shape. Since card 302 the
    // whole route is retired: speed and pause are not settings any more, so
    // the body is accepted, nothing changes, and the answer says so.
    let play = post(at, "/api/v1/set_playback", r#"{"paused":true,"speed":2.0,"fps":60.0}"#).await;
    assert_eq!(play.status, 200, "{}", String::from_utf8_lossy(&play.body));
    let play = play.json();
    assert_eq!(play["paused"], false, "pause is retired: an older client cannot stop the panel");
    assert_eq!(play["speed"], 1.0, "speed is retired: there is one speed");
    assert_eq!(play["fps"], 30.0);
    assert!(play["ignored"].as_str().is_some_and(|s| s.contains("card 302")), "and the answer says why: {play}");
    studio.stop().await;
}

/// Card 163: a parameter whose values are a list of named stops says so in
/// `bootstrap`, and setting it is still setting a number.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parameter_that_is_a_list_of_choices_carries_its_names() {
    let studio = studio().await;
    let at = studio.addr;

    let boot = get(at, "/api/v1/bootstrap").await.json();
    let patch = boot["patches"]
        .as_array()
        .expect("patches")
        .iter()
        .find(|p| p["id"] == "clocks-numerals")
        .expect("clocks-numerals")
        .clone();
    let param = |id: &str| {
        patch["params"]
            .as_array()
            .expect("params")
            .iter()
            .find(|p| p["id"] == id)
            .unwrap_or_else(|| panic!("{id}"))
            .clone()
    };

    // The author's example: five named treatments, not a slider with the list
    // in its label.
    let rest = param("rest");
    assert_eq!(rest["label"], "Resting dials", "the label is a label again");
    assert_eq!(
        rest["choices"],
        serde_json::json!(["as it was", "quiet", "hatched, quiet", "hatched, faint", "zigzag, quiet"])
    );
    assert_eq!(rest["switch"], false);
    assert_eq!((rest["min"].as_f64(), rest["max"].as_f64(), rest["default"].as_f64()), (Some(0.0), Some(4.0), Some(2.0)));

    // The fourteen choreographies the label could not even try to name.
    assert_eq!(param("dance")["choices"].as_array().expect("choices").len(), 14);
    // A switch, not a two-stop slider.
    assert_eq!(param("hours24")["switch"], true);
    // And an ordinary number is untouched.
    let pace = param("pace");
    assert_eq!(pace["choices"], serde_json::json!([]));
    assert_eq!(pace["switch"], false);

    // The value is still an `f32` on the wire and in the state: nothing about
    // a choice changes how it is set or stored.
    assert_eq!(post(at, "/api/v1/set_patch", r#"{"id":"clocks-numerals"}"#).await.status, 200);
    let set = post(at, "/api/v1/set_param", r#"{"id":"rest","value":4.0}"#).await.json();
    assert_eq!(set["params"]["rest"], 4.0);
    let clamped = post(at, "/api/v1/set_param", r#"{"id":"rest","value":9.0}"#).await.json();
    assert_eq!(clamped["params"]["rest"], 4.0, "out of range is clamped by the spec, as it always was");
    studio.stop().await;
}

/// Card 151 took the seed's number off the page; card 301 took the Another
/// button that was left of it off too - "zero people understand it", the
/// owner said. Nothing on either screen is a control for the seed any more,
/// and there is no keyboard shortcut for a button that does not exist.
#[test]
fn the_seed_is_not_a_control_on_the_page_at_all_any_more() {
    for (what, html) in [("index.html", INDEX_HTML), ("panel.html", PANEL_HTML), ("settings.html", SETTINGS_HTML)] {
        for gone in [r#"id="seed""#, r#"id="ro-seed""#, r#"id="new-seed""#, r#"id="another""#] {
            assert!(!html.contains(gone), "{what} still has {gone}: the seed is not a control for humans");
        }
        assert!(!html.contains("<dt>Seed</dt>"), "{what} still reads the seed out in its title block");
    }
    for (what, js) in [("picture.js", PICTURE_JS), ("panel.js", PANEL_JS), ("settings.js", SETTINGS_JS)] {
        assert!(!js.contains("'set_seed'"), "{what} still calls set_seed: nothing in the browser does any more");
        assert!(!js.contains("anotherButton"), "{what} still has the Another button's own state");
    }
    // The Panel screen still says which setting a panel is on, which is what
    // the seed's readout became under card 151.
    assert!(PANEL_HTML.contains(r#"id="ro-setting""#), "the Panel screen says which setting it is on");
}

/// Card 151: the settings control heads the Parameters section, has everything
/// the card asks for, and asks nothing of the browser that cannot be driven or
/// styled - no `prompt()`, no `confirm()`.
#[test]
fn the_settings_control_heads_the_parameters_it_holds() {
    for id in [
        "setting", "setting-list", "setting-mark", "setting-save", "setting-saveas", "setting-rename",
        "setting-delete", "setting-revert", "setting-name", "setting-name-input", "setting-confirm",
        "setting-confirm-yes", "setting-error",
    ] {
        assert!(INDEX_HTML.contains(&format!("id=\"{id}\"")), "the settings control needs #{id}");
    }
    // At the head of the parameters, inside their section.
    let section = INDEX_HTML.find(r#"id="sec-params""#).expect("the parameters section");
    let control = INDEX_HTML.find(r#"id="setting""#).expect("the settings control");
    let params = INDEX_HTML.find(r#"id="params""#).expect("the parameters themselves");
    assert!(section < control && control < params, "the control belongs between the heading and the parameters");

    // Inline, both of them: these two dialogs cannot be driven by the browser
    // tooling, cannot be styled, and stop the page. (The Panel screen still
    // uses `window.confirm` for rename / reboot / forget - card 198's code,
    // and card 159's to fix. This is the rule for the Picture screen, where
    // card 151 put the control it is about.)
    for bad in ["prompt(", "confirm("] {
        for (what, js) in [("common.js", COMMON_JS), ("picture.js", PICTURE_JS)] {
            assert!(!js.contains(bad), "{what} must not use {bad}): the card asks for it inline");
        }
    }
    // "Reset" became "load Default", so the button is gone and the list has it.
    assert!(!INDEX_HTML.contains(r#"id="reset-params""#), "Reset is loading Default now");
    assert!(STYLE_CSS.contains(".setting__actions"), "the control has to look like part of the page");
}

/// And the page's one hard-coded name is the server's own: `Default` is
/// synthesised rather than stored, so the two have to agree on how it is
/// spelled. Same for how long a name may be.
#[test]
fn the_page_and_the_server_agree_about_default_and_about_names() {
    assert!(
        PICTURE_JS.contains(&format!("const DEFAULT_SETTING = '{}'", screeny_studio::state::DEFAULT_SETTING)),
        "the page's name for the patch's own setting must be the server's"
    );
    assert!(
        INDEX_HTML.contains(&format!(r#"maxlength="{}""#, screeny_studio::state::MAX_NAME_CHARS)),
        "the name box must stop where the server does"
    );
}

/// The whole of it over the API: save, load, modified, rename, delete - and the
/// list, the name and the mark travelling with the state, so no browser needs a
/// second read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_setting_is_saved_loaded_and_marked_over_the_api() {
    let studio = studio().await;
    let at = studio.addr;

    assert_eq!(post(at, "/api/v1/set_patch", r#"{"id":"metaballs"}"#).await.status, 200);
    let state = post(at, "/api/v1/set_param", r#"{"id":"count","value":8}"#).await.json();
    assert_eq!(state["setting"], "Default", "everything starts on the patch's own setting");
    assert_eq!(state["settings"], serde_json::json!([]));
    assert_eq!(state["modified"], true, "and a tuned patch is not it any more");

    let saved = post(at, "/api/v1/settings/save", r#"{"name":"  Lava  "}"#).await.json();
    assert_eq!(saved["setting"], "Lava", "trimmed");
    assert_eq!(saved["settings"], serde_json::json!(["Lava"]), "the list comes with the state");
    assert_eq!(saved["modified"], false);

    // A second setting, then back to the first: one change, and the whole
    // working copy moves with it.
    assert_eq!(post(at, "/api/v1/set_param", r#"{"id":"count","value":3}"#).await.status, 200);
    assert_eq!(post(at, "/api/v1/set_playback", r#"{"paused":false,"speed":0.25,"fps":30}"#).await.status, 200);
    let ink = post(at, "/api/v1/settings/save", r#"{"name":"Slow ink"}"#).await.json();
    assert_eq!(ink["settings"], serde_json::json!(["Lava", "Slow ink"]));

    let back = post(at, "/api/v1/settings/load", r#"{"name":"Lava"}"#).await.json();
    assert_eq!(back["params"]["count"], 8.0, "the parameters came back");
    assert_eq!(back["speed"], 1.0, "and so did the speed, which is part of a setting");
    assert_eq!(back["setting"], "Lava");
    assert_eq!(back["modified"], false);

    // Move one thing: the mark, and nothing else, changes.
    let moved = post(at, "/api/v1/set_param", r#"{"id":"count","value":5}"#).await.json();
    assert_eq!(moved["setting"], "Lava");
    assert_eq!(moved["modified"], true);
    // Revert is loading the same name again.
    let reverted = post(at, "/api/v1/settings/load", r#"{"name":"Lava"}"#).await.json();
    assert_eq!(reverted["params"]["count"], 8.0);
    assert_eq!(reverted["modified"], false);

    // Rename follows the working copy; delete leaves it playing.
    let renamed = post(at, "/api/v1/settings/rename", r#"{"to":"Lava lamp"}"#).await.json();
    assert_eq!(renamed["setting"], "Lava lamp");
    assert_eq!(renamed["settings"], serde_json::json!(["Lava lamp", "Slow ink"]));
    assert_eq!(renamed["modified"], false);

    let deleted = post(at, "/api/v1/settings/delete", r#"{}"#).await.json();
    assert_eq!(deleted["setting"], "Default", "the name it came from is gone");
    assert_eq!(deleted["params"]["count"], 8.0, "what is playing did not change");
    assert_eq!(deleted["modified"], true, "which is honestly not Default");
    assert_eq!(deleted["settings"], serde_json::json!(["Slow ink"]));

    // Loading Default is what "Reset" became.
    let default = post(at, "/api/v1/settings/load", r#"{"name":"Default"}"#).await.json();
    assert_eq!(default["modified"], false);
    assert_eq!(default["speed"], 1.0);
    let boot = get(at, "/api/v1/bootstrap").await.json();
    let count = boot["patches"]
        .as_array()
        .expect("patches")
        .iter()
        .find(|p| p["id"] == "metaballs")
        .and_then(|p| p["params"].as_array().cloned())
        .expect("metaballs")
        .into_iter()
        .find(|p| p["id"] == "count")
        .expect("count");
    assert_eq!(default["params"]["count"], count["default"], "Default is the patch's own defaults");

    // Settings belong to the patch, not to the panel: another patch has its own
    // (none), and coming back finds them again.
    let other = post(at, "/api/v1/set_patch", r#"{"id":"clocks-dials"}"#).await.json();
    assert_eq!(other["settings"], serde_json::json!([]), "a setting belongs to its patch");
    let again = post(at, "/api/v1/set_patch", r#"{"id":"metaballs"}"#).await.json();
    assert_eq!(again["settings"], serde_json::json!(["Slow ink"]));
    studio.stop().await;
}

/// Every refusal is a 400 with a sentence a person can read, because the page
/// puts it straight on the line beside the control.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_setting_that_cannot_be_saved_says_why_in_words() {
    let studio = studio().await;
    let at = studio.addr;
    assert_eq!(post(at, "/api/v1/set_patch", r#"{"id":"metaballs"}"#).await.status, 200);
    post(at, "/api/v1/settings/save", r#"{"name":"Lava"}"#).await;

    let long = "x".repeat(screeny_studio::state::MAX_NAME_CHARS + 1);
    for (body, expect) in [
        (r#"{"name":""}"#, "cannot be written over"),      // Save on Default
        (r#"{"name":"   "}"#, "cannot be written over"),   // ...and a name of spaces is no name
        (r#"{"name":"Default"}"#, "patch's own setting"),
        (r#"{"name":"lava"}"#, "already a setting called `Lava`"),
        (&format!(r#"{{"name":"{long}"}}"#), "at most 40 characters"),
    ] {
        // Every one of these is asked while the working copy is on `Lava`...
        post(at, "/api/v1/settings/load", r#"{"name":"Lava"}"#).await;
        // ...except the two that are about Save-on-Default, which need Default.
        if expect == "cannot be written over" {
            post(at, "/api/v1/settings/load", r#"{"name":"Default"}"#).await;
        }
        let r = post(at, "/api/v1/settings/save", body).await;
        assert_eq!(r.status, 400, "{body} should be refused, not accepted");
        let said = r.json()["error"].as_str().unwrap_or_default().to_string();
        assert!(said.contains(expect), "{body} said `{said}`, which does not mention {expect}");
        assert!(said.ends_with('.'), "a refusal is a sentence: `{said}`");
    }

    // And the ones that are about a setting that is not there.
    for (route, body) in [
        ("settings/load", r#"{"name":"Nope"}"#),
        ("settings/rename", r#"{"from":"Nope","to":"Fine"}"#),
        ("settings/delete", r#"{"name":"Nope"}"#),
    ] {
        let r = post(at, &format!("/api/v1/{route}"), body).await;
        assert_eq!(r.status, 400, "{route}");
        assert!(r.json()["error"].as_str().unwrap_or_default().contains("Nope"), "{route}: {}", r.json());
    }
    // Default is nobody's to rename or delete, whatever it is called.
    for body in [r#"{"from":"default","to":"Mine"}"#] {
        assert_eq!(post(at, "/api/v1/settings/rename", body).await.status, 400);
    }
    studio.stop().await;
}

/// Card 151: `bootstrap` says, per patch, whether "another one like this" means
/// anything - and it is the patch's own answer, not a list the page keeps.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_says_which_patches_are_seeded() {
    let studio = studio().await;
    let boot = get(studio.addr, "/api/v1/bootstrap").await.json();
    let patches = boot["patches"].as_array().expect("a list of patches");
    assert!(patches.iter().all(|p| p["seeded"].is_boolean()), "every patch answers: {boot}");
    for p in patches {
        let id = p["id"].as_str().expect("an id");
        let def = screeny_art::patch::find(id).expect("a patch this build has");
        assert_eq!(p["seeded"], def.seeded, "{id}");
    }
    // The two the card names, so that a change of heart has to be deliberate.
    let seeded = |id: &str| patches.iter().find(|p| p["id"] == id).unwrap_or_else(|| panic!("{id}"))["seeded"].clone();
    assert_eq!(seeded("metaballs"), true, "a new seed is a different dance of the same blobs");
    assert_eq!(seeded("vesta"), false, "vesta's picture is the time; the number has nothing to change");
    assert_eq!(seeded("clocks-numerals"), false, "the clocks offer better words of their own");
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
        "the narrow layout must not be a grid with a fixed side column - that is the ~600 px overlap the author saw"
    );
}

/// Each screen stacks in the order it should. That is DOM order, so it is what
/// a narrow window gets with no CSS help at all.
#[test]
fn each_screen_is_in_the_order_it_should_stack_in() {
    let order = |what: &str, html: &'static str, ids: &[(&str, &str)]| {
        let at = |needle: &str| html.find(needle).unwrap_or_else(|| panic!("{what}: `{needle}` is in the page"));
        let found: Vec<(&str, usize)> = ids.iter().map(|(name, id)| (*name, at(id))).collect();
        for pair in found.windows(2) {
            assert!(pair[0].1 < pair[1].1, "{what}: {} should come before {}", pair[0].0, pair[1].0);
        }
    };
    // The Picture screen: the picture, View folded under it, then what
    // changes the picture. Card 301 took brightness, the panel model, the
    // limiter, Speed and pause/restart off this screen entirely.
    order("the Picture screen", INDEX_HTML, &[
        // Card 351: the panel row heads it, then the chosen panel's picture.
        ("the panel row", "id=\"panels\""),
        ("the picture", "id=\"stage\""),
        ("view", "id=\"sec-view\""),
        ("now playing", "id=\"sec-now\""),
        ("parameters", "id=\"sec-params\""),
        // And the meters, which are a detail, come after all of them.
        ("the meters", "class=\"meters\""),
    ]);
    // The Panel screen: the things the owner touches - the switch, brightness,
    // output settings - then what it says about itself, then the studio.
    order("the Panel screen", PANEL_HTML, &[
        ("the panel row", "id=\"panels\""),
        ("the panel's name", "id=\"panel-name\""),
        ("the panel's controls", "id=\"sec-panel\""),
        ("output settings", "id=\"sec-output\""),
        ("the link", "id=\"sec-link\""),
        ("the device's own facts", "id=\"device-block\""),
        ("the studio", "id=\"sec-studio\""),
    ]);
    // The Settings screen (card 311): Home Assistant, and nothing else yet.
    order("the Settings screen", SETTINGS_HTML, &[("Home Assistant", "id=\"sec-ha\"")]);
}

/// Card 301: one nav, the same markup, on every screen - and the current
/// screen is the one it marks, so a person always knows where they are and can
/// always get to the other two. Real navigation: an `href` to a real URL, not
/// a tab that changes what the same document shows.
#[test]
fn the_nav_is_on_every_screen_with_the_current_one_marked() {
    for (what, html, current) in [
        ("index.html", INDEX_HTML, "Picture"),
        ("panel.html", PANEL_HTML, "Panel"),
        ("settings.html", SETTINGS_HTML, "Settings"),
    ] {
        let nav_start = html.find(r#"<nav class="nav""#).unwrap_or_else(|| panic!("{what} should carry the nav"));
        let nav = &html[nav_start..nav_start + html[nav_start..].find("</nav>").expect("a closed nav")];
        assert_eq!(nav.matches("<a ").count(), 3, "{what}: the nav should have exactly three links: {nav}");
        for (dest, label) in [("/", "Picture"), ("/panel", "Panel"), ("/settings", "Settings")] {
            assert!(nav.contains(&format!(r#"href="{dest}""#)), "{what}: the nav should link to {dest}");
            assert!(nav.contains(&format!(">{label}<")), "{what}: the nav should say {label}");
        }
        // Exactly one item is marked, and it is the screen we are actually on.
        assert_eq!(html.matches("aria-current=\"page\"").count(), 1, "{what}: exactly one nav item is current");
        let at = html.find("aria-current=\"page\"").expect("the marked link");
        let tag = &html[html[..at].rfind('<').expect("the tag it is on")..html[at..].find("</a>").map(|n| at + n).expect("its close")];
        assert!(tag.contains(current), "{what}: the marked nav item should say {current}, not: {tag}");
    }
}

/// Card 351: several panels on the page. The Picture and Panel screens open on
/// a row of them, filled by one shared function; which panel a screen is about
/// is `?panel=` and nothing stored; everything the screen changes names it;
/// thumbnails are the README's cheap recipe and the Panel screen still asks
/// for no pictures.
#[test]
fn the_page_has_a_row_of_panels_and_acts_on_the_one_chosen() {
    for (what, html) in [("index.html", INDEX_HTML), ("panel.html", PANEL_HTML)] {
        assert!(html.contains(r#"<div class="panels__row" id="panels">"#), "{what} carries the panel row");
    }
    assert!(!SETTINGS_HTML.contains(r#"id="panels""#), "Settings is about the studio, not a panel");
    assert!(COMMON_JS.contains("export function panelRow("), "one row, shared");
    assert!(PICTURE_JS.contains("panelRow($('#panels'), { path: '/', current: here, thumbs: true })"), "the Picture screen's has thumbnails");
    assert!(PANEL_JS.contains("panelRow($('#panels'), { path: '/panel', current: attachedId })"), "the Panel screen's does not");

    // Thumbnails: a socket per other panel, four frames a second at most, no
    // repeats, no overview, and the screen's own pace - 0 in a hidden tab.
    let row = &COMMON_JS[COMMON_JS.find("export function panelRow(").unwrap()..];
    assert!(row.contains("const THUMB_FPS = 4;"), "thumbnails at a low rate");
    assert!(row.contains("Math.min(THUMB_FPS, pace())"), "and none while the tab is hidden");
    assert!(row.contains("overview: false, quiet: true"), "a thumbnail's socket carries no overview");
    assert!(row.contains("!mine && Boolean(p.picture)"), "no socket for the panel on screen, nor for an idle one");
    assert!(COMMON_JS.contains("url.searchParams.set('panel', panel)"), "a socket names its panel");
    assert!(COMMON_JS.contains("url.searchParams.set('overview', 'false')"), "and can decline the overview");
    assert!(COMMON_JS.contains("url.searchParams.set('repeat', 'false')"), "and never asks for a repeat");

    // The panel is in the URL, both screens open their socket on it, and a
    // forgotten one lands on the first rather than on an error.
    assert!(COMMON_JS.contains("new URLSearchParams(location.search).get('panel')"), "the choice is the URL's");
    for (what, js) in [("picture.js", PICTURE_JS), ("panel.js", PANEL_JS)] {
        assert!(js.contains("bootstrapFor(chosen)"), "{what} bootstraps the chosen panel");
        assert!(js.contains("{ panel: chosen });"), "{what} opens its socket on it");
        assert!(js.contains("error: () => forgetChoice()"), "{what} leaves a forgotten panel");
        assert!(js.contains("carryPanel("), "{what} carries the panel across the nav");
        assert!(!js.contains("d.attached"), "{what} must not mean 'the first panel' by `attached` any more");
    }
    assert!(!SETTINGS_JS.contains("d.attached"), "nor Settings");
    assert!(SETTINGS_JS.contains("showStudioChip("), "Settings' chip is about every panel");

    // Every change on the Picture screen names its panel; the editor says who
    // shares the picture, offers Detach and "Same as".
    assert!(PICTURE_JS.contains("invoke(cmd, forHere(args))"), "calls carry `panel`");
    for id in ["shared", "shared-who", "detach", "same-as", "same-list", "idle-note"] {
        assert!(INDEX_HTML.contains(&format!("id=\"{id}\"")), "#{id} on the Picture screen");
    }
    assert!(PICTURE_JS.contains("call('same_as', { as: p.device })"), "Same as joins that panel's channel");
    assert!(PICTURE_JS.contains("call('detach', {})"), "Detach splits this one off");
    // The Panel screen's controls act on its panel, never on all of them.
    assert!(PANEL_JS.contains("call('set_output', { output: state.output, panel: attachedId() })"), "output settings are this panel's");
    assert!(PANEL_HTML.contains("<summary>Add a panel</summary>"), "'Change which panel' is gone with the stored focus");
    assert!(!PANEL_HTML.contains("Change which panel"), "and nothing still says it");
}

/// Card 351: the owner asked for every screen to work on a phone. What a text
/// file can hold of that: thumb-sized targets on a touch screen or a narrow
/// one, crisp pixels on every canvas, and a panel row that scrolls sideways
/// instead of pushing the page wider.
#[test]
fn every_screen_is_sized_for_a_thumb() {
    let touch = STYLE_CSS.find("@media (pointer: coarse), (max-width: 700px)").expect("a touch block");
    let block = &STYLE_CSS[touch..touch + STYLE_CSS[touch..].find("\n}\n").expect("closed")];
    for needle in ["min-height: 44px", "height: 44px", "::-webkit-slider-thumb", ".seg span", ".patches span", ".switch input"] {
        assert!(block.contains(needle), "the touch block should size {needle}");
    }
    assert!(STYLE_CSS.contains(".pcard__thumb"), "thumbnails are styled");
    assert!(STYLE_CSS.matches("image-rendering: pixelated").count() >= 2, "the main canvas and the thumbnails keep crisp pixels");
    let row = STYLE_CSS.find(".panels__row {").expect("the row");
    assert!(STYLE_CSS[row..row + 200].contains("overflow-x: auto"), "the row scrolls sideways rather than widening the page");
    for (what, html) in [("index.html", INDEX_HTML), ("panel.html", PANEL_HTML), ("settings.html", SETTINGS_HTML)] {
        assert!(html.contains(r#"<meta name="viewport" content="width=device-width, initial-scale=1">"#), "{what} is laid out for the device's width");
    }
}

/// What the page relies on the server for when a bookmark names a panel that
/// has gone: bootstrap says 404, and the page goes to the first panel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panel_that_is_not_there_is_a_404_the_page_can_leave() {
    let studio = studio().await;
    let at = studio.addr;
    assert_eq!(get(at, "/api/v1/bootstrap").await.status, 200);
    assert_eq!(get(at, "/api/v1/bootstrap?panel=nosuchpanel").await.status, 404);
    // Card 353: a studio with no panel has none to list - and always has
    // Channel 1.
    let panels = get(at, "/api/v1/panels").await.json();
    assert_eq!(panels["panels"], serde_json::json!([]), "{panels}");
    let channels = get(at, "/api/v1/channels").await.json();
    assert_eq!(channels["channels"][0]["name"], "Channel 1", "{channels}");
    assert_eq!(get(at, "/api/v1/bootstrap?channel=9").await.status, 404);
}
