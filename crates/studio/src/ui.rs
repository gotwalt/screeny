//! The front end, compiled into the binary.
//!
//! Three files, listed by name rather than globbed: there is no Node
//! toolchain, no bundler and no build script in this crate, and `include_str!`
//! is a build dependency rustc records for us, so editing `ui/style.css`
//! rebuilds the server and nothing else has to know. `--ui-dir` reads the same
//! names off disk instead, which is what live editing wants.

use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use std::path::PathBuf;

/// `(path, content type, bytes)`. Add a file here when the UI grows one.
const EMBEDDED: &[(&str, &str, &[u8])] = &[
    ("index.html", "text/html; charset=utf-8", include_bytes!("../ui/index.html")),
    ("main.js", "text/javascript; charset=utf-8", include_bytes!("../ui/main.js")),
    ("style.css", "text/css; charset=utf-8", include_bytes!("../ui/style.css")),
];

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
    let path = uri.path().trim_start_matches('/');
    let name = if path.is_empty() { "index.html" } else { path };
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
