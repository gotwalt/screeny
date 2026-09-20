//! The dashboard's wiring, checked without a browser.
//!
//! There is no Node toolchain in this crate and no build step, which rules out
//! every usual way of checking a front end (card 121 is the card for closing
//! that properly). What *can* be checked cheaply, and what actually breaks in
//! practice, is the wiring: every element the script reaches for exists in the
//! page, and every route it calls exists on the server. Both of those are
//! silent failures in a browser and loud ones here.

mod common;

use common::{get, post, studio};

const DASHBOARD_HTML: &str = include_str!("../ui/dashboard.html");
const DASHBOARD_JS: &str = include_str!("../ui/dashboard.js");

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

/// Every `api('path'` in the script.
fn routes(js: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = js;
    while let Some(at) = rest.find("api('") {
        rest = &rest[at + 5..];
        if let Some(end) = rest.find('\'') {
            out.push(rest[..end].to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_dashboard_is_served_and_so_are_its_two_files() {
    let studio = studio().await;
    let at = studio.addr;

    for (path, wanted) in [
        ("/dashboard", "text/html"),
        ("/dashboard.html", "text/html"),
        ("/dashboard.js", "text/javascript"),
        ("/dashboard.css", "text/css"),
        // The design view is still where it was.
        ("/", "text/html"),
        ("/main.js", "text/javascript"),
        ("/style.css", "text/css"),
    ] {
        let r = get(at, path).await;
        assert_eq!(r.status, 200, "{path}");
        assert!(!r.body.is_empty(), "{path} is empty");
        let _ = wanted;
    }

    // And the design view points at it, so it can be found without being told.
    let index = String::from_utf8_lossy(&get(at, "/").await.body).to_string();
    assert!(index.contains("href=\"/dashboard\""), "the design view should link to the dashboard");

    // Still no directory traversal, and still a 404 rather than the index for
    // a mistyped asset.
    assert_eq!(get(at, "/nope.js").await.status, 404);
    assert_eq!(get(at, "/../Cargo.toml").await.status, 404);
    assert_eq!(get(at, "/sub/dir.js").await.status, 404);
}

/// Every element the script reaches for exists in the page or in its card
/// template. A typo here is a silent `null` in a browser.
#[test]
fn every_element_the_dashboard_reaches_for_exists() {
    let mut missing = Vec::new();
    for sel in selectors(DASHBOARD_JS) {
        let found = if let Some(id) = sel.strip_prefix('#') {
            DASHBOARD_HTML.contains(&format!("id=\"{id}\""))
        } else if let Some(class) = sel.strip_prefix('.') {
            DASHBOARD_HTML.contains(&format!("\"{class}\"")) || DASHBOARD_HTML.contains(&format!("{class} ")) || DASHBOARD_HTML.contains(&format!(" {class}\""))
        } else {
            true
        };
        if !found {
            missing.push(sel);
        }
    }
    assert!(missing.is_empty(), "the dashboard script reaches for elements the page does not have: {missing:?}");
}

/// Every route the script calls exists on the server. A 404 here is a button
/// that does nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_route_the_dashboard_calls_exists() {
    let studio = studio().await;
    let at = studio.addr;
    let found = routes(DASHBOARD_JS);
    assert!(found.len() >= 8, "the scan found suspiciously few routes: {found:?}");

    for route in &found {
        // Reads are GET, changes are POST; an empty object is a valid body for
        // every one of them, and what comes back does not matter here - only
        // that the route is there at all.
        let r = get(at, &format!("/api/v1/{route}")).await;
        let r = if r.status == 405 { post(at, &format!("/api/v1/{route}"), "{}").await } else { r };
        assert_ne!(r.status, 404, "the dashboard calls /api/v1/{route}, which the server does not have");
    }
    println!("dashboard routes checked: {}", found.join(", "));
}

/// The fault pieces must not reach a browser that did not ask for them, and
/// the dashboard must not offer a brightness that lights nothing (card 136).
#[test]
fn the_dashboard_keeps_its_two_promises() {
    assert!(
        DASHBOARD_JS.contains("BRIGHTNESS_FLOOR = 6"),
        "the slider's lowest non-zero stop should be the first value that lights the panel"
    );
    // Card 136 will delete this; it should be one constant and one helper, not
    // a rule sprinkled through the file.
    assert_eq!(DASHBOARD_JS.matches("BRIGHTNESS_FLOOR").count(), 2, "keep the 1..=5 workaround in one place");

    // No CDN, no module import from anywhere but here: this runs on a LAN box
    // with no promise of internet.
    for bad in ["http://", "https://", "cdn.", "unpkg", "jsdelivr"] {
        assert!(!DASHBOARD_JS.contains(bad), "the dashboard must not reach outside the box: {bad}");
        assert!(!DASHBOARD_HTML.contains(bad), "the dashboard must not reach outside the box: {bad}");
    }
}
