//! The front end, compiled into the binary.
//!
//! **Three screens** since card 301 (two since card 198), one studio behind
//! them, tied together by one nav that is the same markup on every screen:
//!
//! - `/` - the **Picture**: the canvas, what is playing, its parameters and
//!   the named settings. What changes or judges what the picture looks like,
//!   and nothing else - card 301 took Speed, pause/restart and the seed's
//!   Another button off it (the owner: start/stop was baffling and nobody
//!   understood the seed), and moved brightness, the panel model and the
//!   limiter to the Panel screen, because those are panel settings rather
//!   than ways of judging a patch. View (how *this browser* draws the panel)
//!   stays here, folded into a closed disclosure under the canvas.
//! - `/panel` - the **Panel**: which panel, discovery, the output switch,
//!   brightness, the panel model and the limiter, identify / rename / reboot,
//!   the link, what the device says about itself, and this studio's own
//!   health.
//! - `/schedule` - the **Schedule**: playing patches on a timetable rather
//!   than by hand (card 303 builds it; this card gives it the nav and the
//!   chip so it is a real screen rather than a 404).
//!
//! They are separate documents rather than one document with several views:
//! each screen then holds only its own markup, which is what makes "nothing
//! about devices is on the Picture screen" a fact about the file rather than a
//! CSS rule. All are ordinary URLs, so reload and the back button are the
//! browser's job and not ours. What they share is `style.css` and `common.js`
//! - the socket, the poll, the formatting, and the one judgement of what the
//! panel is doing - so a change made on one screen shows on the others, in
//! another browser, at once: they are the same state stream.
//!
//! `/dashboard` - card 106's separate app, folded into the one page by card
//! 170 - still redirects to `/`, so an old bookmark still works.
//!
//! The files are listed by name rather than globbed: there is no Node
//! toolchain, no bundler and no build script in this crate, and `include_str!`
//! is a build dependency rustc records for us, so editing `ui/style.css`
//! rebuilds the server and nothing else has to know. `--ui-dir` reads the same
//! names off disk instead, which is what live editing wants. The scripts are
//! plain ES modules; the browser fetches `common.js` by itself.

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Redirect, Response};
use std::path::PathBuf;

/// `(path, content type, bytes)`. Add a file here when the UI grows one.
const EMBEDDED: &[(&str, &str, &[u8])] = &[
    ("index.html", "text/html; charset=utf-8", include_bytes!("../ui/index.html")),
    ("panel.html", "text/html; charset=utf-8", include_bytes!("../ui/panel.html")),
    ("schedule.html", "text/html; charset=utf-8", include_bytes!("../ui/schedule.html")),
    ("common.js", "text/javascript; charset=utf-8", include_bytes!("../ui/common.js")),
    ("picture.js", "text/javascript; charset=utf-8", include_bytes!("../ui/picture.js")),
    ("panel.js", "text/javascript; charset=utf-8", include_bytes!("../ui/panel.js")),
    ("schedule.js", "text/javascript; charset=utf-8", include_bytes!("../ui/schedule.js")),
    ("style.css", "text/css; charset=utf-8", include_bytes!("../ui/style.css")),
];

/// Tidy URLs: one per screen. `/panel` and `/panel/` are the same screen, and
/// neither is `/panel.js`, which is a file and keeps its extension.
const PAGES: &[(&str, &str)] = &[("", "index.html"), ("panel", "panel.html"), ("schedule", "schedule.html")];

/// Paths that used to be a page of their own and are now part of `/`.
const FOLDED_IN: &[&str] = &["dashboard", "dashboard.html"];

/// Where the UI is read from: the binary, or a directory being edited.
#[derive(Clone, Debug, Default)]
pub struct Ui {
    pub dir: Option<PathBuf>,
}

impl Ui {
    /// Bytes served in the last version of this crate that was compiled.
    #[must_use]
    pub fn embedded_bytes() -> usize {
        EMBEDDED.iter().map(|(_, _, b)| b.len()).sum()
    }

    async fn file(&self, name: &str) -> Option<(String, Vec<u8>)> {
        match &self.dir {
            Some(dir) => {
                let bytes = tokio::fs::read(dir.join(name)).await.ok()?;
                Some((content_type(name).to_owned(), bytes))
            }
            None => EMBEDDED
                .iter()
                .find(|(p, _, _)| *p == name)
                .map(|(_, ct, bytes)| ((*ct).to_owned(), (*bytes).to_vec())),
        }
    }
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// Everything that is not the API is the UI. Anything unknown is a 404 rather
/// than the index: a mistyped asset should say so, not arrive as HTML.
pub async fn serve(axum::extract::State(st): axum::extract::State<crate::AppState>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/').trim_end_matches('/');
    // The dashboard is the same page now. A permanent redirect would be
    // cached for ever by a browser that had the old bookmark, which is a
    // nuisance the day somebody wants `/dashboard` back for something else.
    if FOLDED_IN.contains(&path) {
        return Redirect::temporary("/").into_response();
    }
    let name = PAGES.iter().find(|(url, _)| *url == path).map_or(path, |(_, file)| *file);
    // A served name is one file in one directory: no traversal, no
    // subdirectories, whether the bytes come from the binary or from disk.
    if name.contains('/') || name.contains('\\') || name.starts_with('.') {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match st.ui.file(name).await {
        // No caching: the point of `--ui-dir` is that a reload shows the edit.
        Some((ct, bytes)) => ([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "no-cache".into())], bytes).into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}
