//! Card 201 spike, part 3: the HTTP server.

use embassy_net::Stack;
use embassy_time::{Duration, Instant};
use log::info;
use picoserve::io::Read;
use picoserve::response::{IntoResponse, ResponseWriter, StatusCode};
use picoserve::routing::{get, get_service, post, PathRouterService, RequestHandlerService};
use picoserve::{ResponseSent, Router};

use super::{HTTP_BUF, HTTP_TASKS, TCP_RX, TCP_TX};
use crate::mk_static;

// ---------------------------------------------------------------------------
// 3: the HTTP server
// ---------------------------------------------------------------------------

/// The one page. `File::html` hashes it at compile time for an ETag, so a
/// phone that has already loaded the portal gets a 304 on the second visit.
const PORTAL_HTML: &str = include_str!("../web_spike_portal.html");

/// `GET /api/v1/status`. The `GET_INFO` and telemetry numbers, as JSON.
#[derive(serde::Serialize)]
struct Status {
    id: &'static str,
    fw: &'static str,
    uptime_ms: u32,
    heap_used: usize,
    heap_size: usize,
    rssi_dbm: i8,
    brightness: u8,
    wifi_state: u8,
    /// The SSID the station is configured for. **Never the PSK** (spec 8.4).
    ssid: heapless::String<32>,
    portal: bool,
}

/// `POST /api/v1/wifi`, urlencoded. Owned `heapless::String` fields, not
/// borrowed `&str`: picoserve's `Form` extractor deserialises through a
/// `Deserializer` whose lifetime is not `'de`-compatible with a borrowing
/// struct, so a borrowed form is rejected at compile time.
#[derive(serde::Deserialize)]
struct WifiForm {
    ssid: heapless::String<32>,
    psk: heapless::String<64>,
}

async fn get_status() -> impl IntoResponse {
    let stats = esp_alloc::HEAP.stats();
    picoserve::response::Json(Status {
        id: "000000",
        fw: crate::FW_VERSION,
        uptime_ms: Instant::now().as_millis() as u32,
        heap_used: stats.current_usage,
        heap_size: stats.size,
        rssi_dbm: crate::RSSI_DBM.load(core::sync::atomic::Ordering::Relaxed),
        brightness: crate::BRIGHTNESS.load(core::sync::atomic::Ordering::Relaxed),
        wifi_state: crate::WIFI_STATE.load(core::sync::atomic::Ordering::Relaxed),
        ssid: heapless::String::new(),
        portal: true,
    })
}

async fn post_wifi(
    picoserve::extract::Form(form): picoserve::extract::Form<WifiForm>,
) -> impl IntoResponse {
    // Never log `form.psk`. Length only, so a support log can still say
    // "you submitted an empty password".
    info!(
        "portal: set-wifi ssid {:?} psk_len {}",
        form.ssid.as_str(),
        form.psk.len()
    );
    // -> card 200's `save_wifi(form.ssid, form.psk)`.
    picoserve::response::Json(("accepted", form.ssid))
}

/// `POST /api/v1/firmware`: the streaming sink.
///
/// A handler *function* cannot take the body, because every `FromRequest`
/// extractor that exists either reads the whole body into the HTTP buffer or
/// parses it. Streaming needs a [`RequestHandlerService`], which is handed the
/// whole `Request` and can call `body_connection.body().reader()`.
struct FirmwareUpload;

impl<State, PathParameters> RequestHandlerService<State, PathParameters> for FirmwareUpload {
    async fn call_request_handler_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        _state: &State,
        _path_parameters: PathParameters,
        request: picoserve::request::Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        let mut body_connection = request.body_connection;
        let body = body_connection.body();
        let total = body.content_length();
        let mut reader = body
            .reader()
            // The default `read_request` timeout is 3 s, which would kill a
            // 1 MB upload over WiFi. This is the API that raises it.
            .with_different_timeout(Duration::from_secs(120));

        let mut chunk = [0u8; 512];
        let mut written = 0usize;
        let mut failed = false;
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => {
                    // -> card 200's `OtaSink::write(&chunk[..n])`
                    written += n;
                }
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        info!("portal: firmware body {} of {} bytes", written, total);

        let connection = body_connection.finalize().await?;
        if failed || written != total {
            (
                StatusCode::BAD_REQUEST,
                "upload truncated\n",
            )
                .write_to(connection, response_writer)
                .await
        } else {
            (StatusCode::OK, "ok\n")
                .write_to(connection, response_writer)
                .await
        }
    }
}

/// The captive-portal catch-all: everything that is not a real route gets a
/// 302 to `http://192.168.4.1/`.
///
/// This is the *fallback* of the router, so it never shadows a real route and
/// therefore cannot redirect the portal page to itself. `Host:` does not need
/// checking for that reason - the loop the OSes get into is a path loop, not a
/// host loop.
struct CaptivePortal;

impl<State, PathParameters> PathRouterService<State, PathParameters> for CaptivePortal {
    async fn call_path_router_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        _state: &State,
        _path_parameters: PathParameters,
        path: picoserve::request::Path<'_>,
        request: picoserve::request::Request<'_, R>,
        response_writer: W,
    ) -> Result<ResponseSent, W::Error> {
        let host = request
            .parts
            .headers()
            .get("host")
            .and_then(|v| v.as_str().ok())
            .unwrap_or("");
        let _ = path;
        info!(
            "portal: redirecting {} host {:?}",
            request.parts.path(),
            host
        );
        (
            StatusCode::new(302),
            ("Location", "http://192.168.4.1/"),
            "http://192.168.4.1/\n",
        )
            .write_to(request.body_connection.finalize().await?, response_writer)
            .await
    }
}

#[embassy_executor::task(pool_size = HTTP_TASKS)]
pub async fn http_task(id: usize, stack: Stack<'static>) -> ! {
    // The router is built here, as a task local, on purpose: naming its type
    // in a `static` needs `#![feature(impl_trait_in_assoc_type)]` (that is
    // what picoserve's `AppBuilder` is for, and its own docs say it "requires
    // the nightly Rust toolchain"). Building it in the task keeps the whole
    // thing on stable.
    let app = Router::from_service(CaptivePortal)
        .route("/", get_service(picoserve::response::File::html(PORTAL_HTML)))
        .route("/api/v1/status", get(get_status))
        .route("/api/v1/wifi", post(post_wifi))
        .route("/api/v1/firmware", picoserve::routing::post_service(FirmwareUpload));

    let config = picoserve::Config::new(picoserve::Timeouts {
        start_read_request: Duration::from_secs(5),
        persistent_start_read_request: Duration::from_secs(2),
        read_request: Duration::from_secs(5),
        write: Duration::from_secs(5),
    })
    .keep_connection_alive();

    let http_buf = mk_static!([u8; HTTP_BUF], [0u8; HTTP_BUF]);
    let rx = mk_static!([u8; TCP_RX], [0u8; TCP_RX]);
    let tx = mk_static!([u8; TCP_TX], [0u8; TCP_TX]);

    picoserve::Server::new(&app, &config, &mut http_buf[..])
        .listen_and_serve(id, stack, 80, &mut rx[..], &mut tx[..])
        .await
        .into_never()
}
