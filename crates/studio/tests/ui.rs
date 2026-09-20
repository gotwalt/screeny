//! The page's wiring, checked without a browser.
//!
//! There is no Node toolchain in this crate and no build step, which rules out
//! every usual way of checking a front end (card 121 is the card for closing
//! that properly). What *can* be checked cheaply, and what actually breaks in
//! practice, is the wiring: every element the script reaches for exists in the
//! page, and every route it calls exists on the server. Both of those are
//! silent failures in a browser and loud ones here.
//!
//! Card 170 folded the dashboard into the one page; card 198 split that page
//! into two **screens** - the Picture at `/` and the Panel at `/panel` - and
//! `/dashboard` still has to work as a bookmark.
//!
//! So the wiring check is now per screen: every element `picture.js` reaches
//! for is in `index.html`, every element `panel.js` reaches for is in
//! `panel.html`, and every element `common.js` reaches for is in **both** -
//! which is the rule that keeps the shared file shared.

mod common;

use common::{get, post, studio};

const INDEX_HTML: &str = include_str!("../ui/index.html");
const PANEL_HTML: &str = include_str!("../ui/panel.html");
const COMMON_JS: &str = include_str!("../ui/common.js");
const PICTURE_JS: &str = include_str!("../ui/picture.js");
const PANEL_JS: &str = include_str!("../ui/panel.js");
const STYLE_CSS: &str = include_str!("../ui/style.css");

/// The whole front end, for the claims that are about it rather than about one
/// screen.
fn all_js() -> String {
    format!("{COMMON_JS}\n{PICTURE_JS}\n{PANEL_JS}")
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
async fn both_screens_are_served_and_so_are_their_files() {
    let studio = studio().await;
    let at = studio.addr;

    // (The content types are `ui::content_type`'s and are checked over real
    // HTTP in the card's Log; the test client keeps only the body.)
    for path in [
        "/", "/index.html", "/panel", "/panel/", "/panel.html",
        "/common.js", "/picture.js", "/panel.js", "/style.css",
    ] {
        let r = get(at, path).await;
        assert_eq!(r.status, 200, "{path}");
        assert!(!r.body.is_empty(), "{path} is empty");
    }

    // `/panel` is a screen and `/panel.js` is a file: the tidy URL must not
    // swallow the script that happens to share its name.
    let screen = String::from_utf8_lossy(&get(at, "/panel").await.body).to_string();
    let script = String::from_utf8_lossy(&get(at, "/panel.js").await.body).to_string();
    assert!(screen.starts_with("<!doctype html>"), "/panel should be the screen");
    assert!(script.starts_with("// The Studio's Panel screen"), "/panel.js should be the script");

    // Each screen loads its own module, and both load the shared one through
    // it rather than with a second <script> tag.
    assert!(screen.contains(r#"src="/panel.js""#), "the panel screen loads its own script");
    assert!(PANEL_JS.contains("from './common.js'"), "...which imports the shared one");

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
        ("common.js", COMMON_JS, &[("index.html", INDEX_HTML), ("panel.html", PANEL_HTML)][..]),
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
/// devices is on the Picture screen** except the one status chip and
/// brightness, and everything that was in the old Panel section is on the
/// Panel screen.
#[test]
fn the_two_screens_hold_what_the_split_says_they_do() {
    // The panel's own affairs, by the ids they are drawn into. Every one of
    // them was on the one page before this card.
    for gone in [
        "discovery-note", "panel-out", "panel-out-label", "panel-facts", "device-block",
        "device-facts", "device-note", "identify", "rename", "reboot", "setup", "found", "add-to",
    ] {
        assert!(!INDEX_HTML.contains(&format!("id=\"{gone}\"")), "#{gone} belongs on the Panel screen");
        assert!(PANEL_HTML.contains(&format!("id=\"{gone}\"")), "#{gone} has to still exist somewhere");
    }
    // Nor is any of it *said* on the Picture screen.
    for word in ["WiFi", "Reboot", "heap", "discovery", "mDNS"] {
        assert!(!INDEX_HTML.contains(word), "the Picture screen should not talk about {word}");
    }
    // The two that stay, and why: brightness changes how the patch looks on
    // the LEDs, and the chip is the link to the other screen.
    assert!(INDEX_HTML.contains("id=\"bright\""), "brightness stays reachable while judging a patch");
    assert!(PANEL_HTML.contains("id=\"bright\""), "and is the same control on the Panel screen");
    assert!(COMMON_JS.contains("export function bindBrightness"), "bound once, so the two cannot drift");
    assert!(
        INDEX_HTML.contains(r#"<a class="pill pill--link" id="ro-panel" href="/panel">"#),
        "the status chip is the way to the Panel screen"
    );
    assert!(PANEL_HTML.contains(r#"<a class="backlink" href="/">"#), "...and there is a way back");

    // The picture is not on the Panel screen at all - no canvas, and so no
    // frames asked for (card 120's rule, restated for a screen that draws
    // none).
    assert!(!PANEL_HTML.contains("<canvas"), "the Panel screen draws no picture");
    assert!(PANEL_JS.contains("}, noFrames);"), "so it must ask the socket for none");
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

/// Both screens keep the promises card 106 made for the dashboard: a
/// brightness that lights nothing is never offered, and nothing reaches
/// outside the box.
#[test]
fn the_screens_keep_their_promises() {
    assert!(
        COMMON_JS.contains("BRIGHTNESS_FLOOR = 6"),
        "the slider's lowest non-zero stop should be the first value that lights the panel"
    );
    // Card 136 will delete this; it should be one constant and one helper, not
    // a rule sprinkled through the front end. Card 198: the brightness control
    // is bound once in `common.js`, so both screens inherit the floor and
    // neither screen's own file mentions it.
    assert_eq!(COMMON_JS.matches("BRIGHTNESS_FLOOR").count(), 2, "keep the 1..=5 workaround in one place");
    assert!(!PICTURE_JS.contains("BRIGHTNESS_FLOOR") && !PANEL_JS.contains("BRIGHTNESS_FLOOR"));

    // No CDN, no web font from the network, no module import from anywhere but
    // here: this runs on a LAN box with no promise of internet.
    for bad in ["http://", "https://", "cdn.", "unpkg", "jsdelivr", "fonts.googleapis"] {
        for (what, text) in [
            ("index.html", INDEX_HTML),
            ("panel.html", PANEL_HTML),
            ("common.js", COMMON_JS),
            ("picture.js", PICTURE_JS),
            ("panel.js", PANEL_JS),
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
    for decided in [
        "f.stack_warn",
        "f.stack_fault",
        "f.low_heap",
        "f.bad_fw_state",
        "f.odd_reset",
        "f.store_errors",
        "f.unasked_reboots",
    ] {
        assert!(PANEL_JS.contains(decided), "the page reads {decided} rather than deciding it");
    }
    let body = device_block();
    // Every threshold that has ever been one, including the ones card 195
    // replaced: none of them belongs in a browser.
    for invented in ["2048", "4096", "8192", "0.85", "< 4312", "98304"] {
        assert!(!body.contains(invented), "the page must not carry its own copy of a threshold: {invented}");
    }
    // The four that are meant to be loud are loud, and nothing else is: a
    // reset that should not have happened, a store error, a stack past the
    // fault line and a heap past it (card 195 made both of those faults).
    assert_eq!(body.matches("'bad'").count(), 4, "the four fault tones, and only those: {body}");
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
    assert!(PANEL_JS.contains("{ on: true, to: '' } : { on: false }"), "`set_panel`'s two bodies are unchanged");
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

/// Card 183: the stops the rate slider declares are **drawn**, and drawn where
/// the thumb actually lands.
///
/// The drift this guards against is the one the card was written about: the
/// mark's position is `3.5px + frac * (W - 7px)`, which is the thumb's own
/// geometry, and the thumb's width lives in `style.css`. If somebody changes
/// the thumb and not the arithmetic, the marks go quietly out of line - worst
/// at the right-hand end, where 60 fps is - and nothing else would say so.
#[test]
fn the_rate_sliders_stops_are_drawn_where_the_thumb_lands() {
    assert!(INDEX_HTML.contains(r#"list="fps-stops""#), "the rate slider still declares its stops");
    assert!(COMMON_JS.contains("function drawStops"), "and something draws them");
    assert!(PICTURE_JS.contains("drawStops)"), "drawStops has to actually be called");
    assert!(STYLE_CSS.contains(".slider .stops"), "the marks need somewhere to be");

    // The thumb, as the stylesheet has it.
    let thumb = STYLE_CSS
        .split_once("::-webkit-slider-thumb {")
        .and_then(|(_, rest)| rest.split_once('}'))
        .map(|(block, _)| block.to_string())
        .expect("a thumb rule");
    let width = thumb
        .split("width:")
        .nth(1)
        .and_then(|w| w.split(';').next())
        .map(str::trim)
        .expect("the thumb's width");
    assert_eq!(width, "7px", "the thumb changed width; the marks' arithmetic has to change with it");
    assert!(
        COMMON_JS.contains("calc(3.5px + ${at} * (100% - 7px))"),
        "the marks must use the thumb's own geometry: 3.5px + frac * (W - 7px)"
    );

    // And every stop is a rate the slider can actually reach.
    let stops = stops_of(INDEX_HTML, "fps-stops");
    assert!(stops.len() >= 4, "found only {stops:?}");
    for stop in &stops {
        assert!(
            (screeny_studio::player::MIN_FPS..=screeny_studio::player::MAX_FPS).contains(stop),
            "{stop} is not a rate the player can be on"
        );
    }
}

/// The values a `<datalist>` declares, in the order it declares them.
fn stops_of(html: &str, list: &str) -> Vec<f64> {
    let rest = html.split_once(&format!(r#"<datalist id="{list}">"#)).unwrap_or_else(|| panic!("{list}")).1;
    rest.split_once("</datalist>")
        .expect("a closed datalist")
        .0
        .split(r#"<option value=""#)
        .skip(1)
        .filter_map(|o| o.split('"').next().and_then(|v| v.parse().ok()))
        .collect()
}

/// Card 197, folded into 198: the Speed slider has a home position.
///
/// It is the general mechanism rather than a second one - a `<datalist>` that
/// `drawStops` draws - so this test is written over **every** slider on either
/// screen that declares stops: each one's stops are inside its own range, and
/// each is in the range's own units rather than a percentage. What is special
/// to Speed is the way *back*: the marks do not snap (a magnet at 1.00 would
/// make 0.95 and 1.05 unreachable with a mouse, and a speed a script set must
/// be shown exactly), so a double-click returns it to 1.00x.
#[test]
fn every_slider_that_declares_stops_declares_reachable_ones() {
    let mut checked = 0;
    for (what, html) in [("index.html", INDEX_HTML), ("panel.html", PANEL_HTML)] {
        for decl in html.split(r#"list=""#).skip(1) {
            let list = decl.split('"').next().expect("a list name");
            // The input's own range, from the tag the `list=` is in.
            let tag = html.split_once(&format!(r#"list="{list}""#)).expect("the input").0;
            let tag = &tag[tag.rfind("<input").expect("an input tag")..];
            let attr = |name: &str| -> f64 {
                tag.split_once(&format!(r#"{name}=""#))
                    .and_then(|(_, r)| r.split('"').next())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| panic!("{what}: {list}'s input has no {name}"))
            };
            let (min, max) = (attr("min"), attr("max"));
            let stops = stops_of(html, list);
            assert!(!stops.is_empty(), "{what}: {list} declares no stops");
            for stop in &stops {
                assert!((min..=max).contains(stop), "{what}: {list} declares {stop}, outside {min}..={max}");
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 2, "the rate slider and the speed slider declare stops");

    // Speed's own three, and the way home.
    assert_eq!(stops_of(INDEX_HTML, "speed-stops"), vec![0.5, 1.0, 2.0], "0.5x, 1.00x and 2x");
    assert!(INDEX_HTML.contains(r#"list="speed-stops""#), "the speed slider declares them");
    assert!(PICTURE_JS.contains("speedInput.addEventListener('dblclick'"), "a double-click goes home");
    assert!(PICTURE_JS.contains("state.speed = 1;"), "...to 1.00x");
    // And still no snapping, on either slider: what draws the marks never
    // touches the value.
    let start = COMMON_JS.find("export function drawStops").expect("drawStops");
    let body = &COMMON_JS[start..start + COMMON_JS[start..].find("\n}").expect("its end")];
    assert!(!body.contains("input.value"), "the stops are marks, not magnets: {body}");
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

    // The owner's example: five named treatments, not a slider with the list
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

/// Card 151: **the seed is not a control for humans.** The number is gone from
/// both screens - the readout in the title block and the numeric input - and
/// what is left of it is one quiet button for a patch whose picture actually
/// depends on it.
#[test]
fn the_seeds_number_is_not_on_the_page_any_more() {
    for (what, html) in [("index.html", INDEX_HTML), ("panel.html", PANEL_HTML)] {
        for gone in [r#"id="seed""#, r#"id="ro-seed""#, r#"id="new-seed""#] {
            assert!(!html.contains(gone), "{what} still has {gone}: the seed is not a control for humans");
        }
        assert!(!html.contains("<dt>Seed</dt>"), "{what} still reads the seed out in its title block");
    }
    // What replaced it on each screen: the Another button here, the setting's
    // name there.
    assert!(INDEX_HTML.contains(r#"id="another""#), "the Picture screen keeps one quiet Another button");
    assert!(PANEL_HTML.contains(r#"id="ro-setting""#), "the Panel screen says which setting it is on instead");
    assert!(PICTURE_JS.contains("patch.seeded"), "and the button is shown from the patch's own flag");
    // The number itself is still reachable for somebody reproducing a frame.
    assert!(PICTURE_JS.contains("Seed ${state.seed}"), "the tooltip still carries the number");
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
    let other = post(at, "/api/v1/set_patch", r#"{"id":"plasma"}"#).await.json();
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
    assert_eq!(post(at, "/api/v1/set_patch", r#"{"id":"plasma"}"#).await.status, 200);
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
    assert_eq!(seeded("plasma"), true, "a new seed is a different plasma");
    assert_eq!(seeded("testcard"), false, "the test card ignores its seed");
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
        "the narrow layout must not be a grid with a fixed side column - that is the ~600 px overlap the owner saw"
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
    // The Picture screen: the picture, then what changes it, then - since card
    // 198 - brightness, which is the one panel control that judges a picture.
    order("the Picture screen", INDEX_HTML, &[
        ("the picture", "id=\"stage\""),
        ("now playing", "id=\"sec-now\""),
        ("parameters", "id=\"sec-params\""),
        ("brightness", "id=\"sec-bright\""),
        ("time", "id=\"sec-time\""),
        // And the meters, which are a detail, come after all of them.
        ("the meters", "class=\"meters\""),
    ]);
    // The Panel screen: which panel and its controls, then what it says about
    // itself, then what the studio says about itself.
    order("the Panel screen", PANEL_HTML, &[
        ("the panel's name", "id=\"panel-name\""),
        ("the panel's controls", "id=\"sec-panel\""),
        ("the link", "id=\"sec-link\""),
        ("the device's own facts", "id=\"device-block\""),
        ("the studio", "id=\"sec-studio\""),
    ]);
}
