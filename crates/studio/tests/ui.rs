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
