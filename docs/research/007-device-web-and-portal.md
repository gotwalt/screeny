# 007 - A web server, a soft-AP and a captive portal on the firmware's stack

Card 201. Research only: nothing here is flashed, nothing here is on `main`'s
firmware. Every crate claim is a line I read in `~/.cargo/registry` or in the
unpacked `.crate`, at the exact version we can use; every size is a number the
Xtensa toolchain printed for a build in this worktree. Sibling cards 200 (flash,
the settings store, OTA) and 202 (the button) own everything I leave to them.

---

## Conclusions first

1. **HTTP server: `picoserve 0.20.0`.** It depends on `embassy-net ^0.9.1`,
   `embassy-time ^0.5.1`, `heapless 0.9.3` and `embedded-io-async 0.7` - our
   exact pins, no resolver pressure at all. It is `no_std`, no-alloc, has an
   axum-shaped router, `serde` JSON in and out via `serde-json-core`, a
   compile-time-ETag static-file response, and - the thing that decides it - a
   **streaming request-body reader with a per-request timeout override**, which
   is what a 1 MB firmware upload needs. `edge-http 0.8.0` also fits our
   `edge-nal 0.7` family and is a perfectly good lower-level HTTP engine, but it
   gives you a `Handler` trait and nothing above it: routing, form parsing,
   JSON and the file response would all be ours to write. We already carry the
   edge-* family for mDNS, so this is not a "one implementation of each thing"
   violation - edge-* is our *DNS-SD/DHCP/DNS* family and picoserve is the HTTP
   one; they do not overlap.
2. **APSTA, not exclusive modes.** `esp-radio 1.0.0-beta.1` has
   `Config::AccessPointStation(StationConfig, AccessPointConfig)` and a second
   `Interface::access_point()` singleton, and the two interfaces each drive
   their own `embassy_net::Stack`. AP-only mode **cannot scan** - and the
   settings page has to show a network list - so exclusive AP mode is out on
   function, before RAM enters the argument. The price is the ESP32's
   single-PHY rule: the soft-AP is dragged onto the station's channel the moment
   the station associates, announced by a CSA the phone may or may not follow.
3. **DHCP and DNS: `edge-dhcp 0.8.0` and `edge-captive 0.8.0`.** Same
   `edge-nal 0.7` + `domain 0.12` family as the `edge-mdns 0.8` we already ship,
   so `domain`'s message machinery is compiled in once. `edge-dhcp 0.8`'s server
   runs over a **plain UDP socket** - earlier versions needed a raw socket, which
   `edge-nal-embassy` does not have - and already emits RFC 8910 option 114.
4. **State machine, in one paragraph.** Boot -> if the store has credentials,
   join with 3 attempts over ~45 s; on success go `ONLINE` (AP down, LAN HTTP
   server up). On failure, or with no stored credentials, go `PORTAL`: raise the
   soft-AP `screeny-<id>` in APSTA, put the QR screen on the panel, serve DHCP,
   the DNS catch-all and the portal. A submitted credential does **not** reboot
   and does **not** commit: it goes to `TRIAL`, which keeps the AP up, tries the
   network, and answers a *full-page reload* of the portal with "connected, find
   me at 192.168.x.y" or "failed: wrong password / network not found" - then
   commits and drops the AP only on success. `PORTAL` is never terminal: a
   `RETRY` tick every 10 minutes re-tries the stored credentials while no client
   is associated to the AP, so the 3 a.m. router reboot heals itself. Telemetry
   byte 46 reads `PROVISIONING` (4) in `PORTAL` and `TRIAL`; `GET_WIFI`'s state
   byte reads `CONNECTING` in `TRIAL`, `FAILED` after a failed trial,
   `CONNECTED` in `ONLINE`.
5. **The measured cost, and the headline risk.** The full spike adds
   **+112,080 bytes of flash** (743,408 -> 855,488, +15%, 20.7% of a 4 MB app
   slot) and **+23,160 bytes of `.bss`**. But `.bss` and core 0's main stack
   share one region, so `.stack` fell from **37,536 to 13,688 bytes** - and
   `main` builds two 12 KB `FrameBuffer` temporaries on that stack. **This build
   would very likely panic on the stack guard at boot.** The RAM budget, not any
   API, is the hard part of this card. Section 6 says exactly what has to move.
6. **QR: yes, version 2-L, and the SSID must stay <= 14 characters.** The owner
   measured `WIFI:T:nopass;S:screeny-4a00a4;;` (exactly 32 bytes, version 2-L,
   25x25) scanning easily on the panel at one LED per module with a 3-pixel lit
   quiet zone. The block is then 31x31, which leaves 32 of 64 columns - eight
   `FONT_4X6` characters - for the name. `screeny-4a00a4` is *exactly* at the
   32-byte limit: a 15-character SSID needs version 3 (29x29), which with a
   1-pixel quiet zone is 31 of 32 rows and leaves no room for text at all.
   `qrcodegen-no-heap 1.8.1` encodes it with no allocator in two 107-byte
   buffers and costs ~10.5 KB of flash.
7. **Nothing here may cost a frame, and nothing here should.** Everything on
   core 0 is one cooperative executor, so an HTTP request does share time with
   the frame task - but the portal only exists when the device is *not* being
   streamed to, the display is on core 1 behind a circular DMA that keeps
   scanning with no CPU, and the LAN web server is one worker with a short
   keep-alive. The AP and STA have **separate driver RX queues**, so AP traffic
   cannot steal frame-path RX slots. The acceptance test is telemetry, not
   argument: `interarrival_us` / `jitter_us` / `frames_dropped_*` under `wrk`.

---

## 1. The HTTP server

### 1.1 What is already in the image

`firmware/Cargo.toml:81` takes `edge-nal-embassy = "=0.9.0"` **with default
features**, and that crate's default is `all`, which includes
`tcp = ["embassy-net/tcp"]` (its `Cargo.toml`, `[features]`). So smoltcp's TCP
implementation, its DNS socket and IPv6 are *already compiled into `main`'s
image*. Turning on an HTTP server is therefore not "adding TCP"; most of that
cost is already paid. `firmware/Cargo.toml:58` should still gain `"tcp"`
explicitly, because relying on a transitive feature for something load-bearing
is how a future `default-features = false` silently breaks the build.

`embassy-net 0.9.1` always adds a DNS socket to the `SocketSet`
(`src/lib.rs:350`) and adds a DHCPv4 socket the first time DHCP is configured
(`src/lib.rs:707-710`). The STA stack's `StackResources<6>`
(`firmware/src/main.rs:600`) is therefore already holding DNS + DHCP + frames +
control + mDNS = **5 of 6**. A TCP listener on the LAN side needs the sixth and
leaves no spare; the build card should raise it to `StackResources<8>`.

### 1.2 picoserve 0.20.0 vs edge-http 0.8.0

| | `picoserve 0.20.0` | `edge-http 0.8.0` |
|---|---|---|
| `no_std` / no alloc | yes (`alloc` is an opt-in feature we do not enable) | yes |
| embassy-net 0.9.1 | `embassy-net = "0.9.1"` with `tcp, proto-ipv4, medium-ethernet`, `embassy-time = "0.5.1"` (its `Cargo.toml`) | via `edge-nal 0.7` + `edge-nal-embassy 0.9` |
| MSRV | 1.93 - the `esp` toolchain here is `rustc 1.97.0-nightly (8ea53bcd7 2026-07-08)` | 1.91-ish |
| Transport | its own `listen_and_serve(stack, port, rx, tx)` over `embassy_net::tcp::TcpSocket` (`src/lib.rs:703-726`) | `edge_nal::TcpAccept`, i.e. `edge_nal_embassy::Tcp` + a `TcpBuffers` pool (`edge-nal-embassy/src/tcp.rs:25,92`) |
| Routing | axum-shaped `Router::route("/path", get(handler))`, path parameters, nesting, layers | none - you implement `Handler` and match on `headers.path` yourself |
| Extractors / forms / JSON | `extract::Form`, `extract::Json`, `response::Json` (serde + `serde-json-core`) | none |
| Static file | `response::File::html(&'static str)` with a **compile-time SHA-1 ETag** and a 304 on `If-None-Match` (`src/response/fs.rs:91-106,143-170`) | none |
| Streaming request body | **yes** - `RequestBody::reader()` (`src/request.rs:569`) returns a `Read` over the body, and `with_different_timeout(Duration)` (`src/request.rs:483`) replaces the 3 s default just for that read | yes - `Body` is a `Read` |
| Chunked request bodies | **no** - only `content-length` is parsed (`src/request.rs:995-998`) | yes (`body_type`) |
| Concurrency | one `listen_and_serve` task per connection; N tasks = N connections (`pool_size`) | `Server<P, B, N>` owns P buffers of B bytes and runs P handler tasks (`src/io/server.rs:16-17,643,649`) |
| Keep-alive | `Config::keep_connection_alive()`, per-request `Connection:` honoured (`src/lib.rs:174-197,226`) | `keepalive_timeout_ms` argument |
| Timeouts | built in: `start_read_request` 5 s, `persistent_start_read_request` 1 s, `read_request` 3 s, `write` 1 s (`src/lib.rs:127-134`) | **none by default**; you wrap with `edge_nal::with_timeout` |

**Take picoserve.** The deciding pair is the streaming body reader *with a
timeout override* and the fact that the router, the form parser and the JSON
encoder are all already written and `no_std`. edge-http's defaults would also
have to be re-tuned hard: `DEFAULT_HANDLER_TASKS_COUNT = 4` and
`DEFAULT_BUF_SIZE = 2048` is 8 KB of buffer in the `Server` struct alone
(`src/io/server.rs:16,17,643`), before `TcpBuffers`.

Two picoserve facts the build card must know, both found by the compiler:

- **Handler functions cannot take the body.** Every `FromRequest` extractor
  either `read_all()`s the body into the HTTP buffer or parses it. Streaming
  needs a `RequestHandlerService` implementation, routed with `post_service(..)`;
  it is handed the whole `Request` and can call
  `request.body_connection.body().reader()`. The spike does exactly this in
  `firmware/src/web_spike/http.rs` (`struct FirmwareUpload`).
- **`extract::Form` cannot deserialise into a borrowing struct.** A
  `struct WifiForm<'a> { ssid: &'a str }` fails with "implementation of
  `Deserialize` is not general enough". Use owned `heapless::String<N>` fields;
  `picoserve` already depends on `heapless` with `serde`.
- **No `#![feature(impl_trait_in_assoc_type)]` needed** if the `Router` is built
  as a *task local* rather than named in a `static`. picoserve's `AppBuilder`
  trait is the nightly-only convenience; the spike avoids it and stays on what
  stable would accept. Worth keeping: the esp toolchain happens to be nightly,
  but the firmware should not depend on that.

### 1.3 The captive-portal redirect, in picoserve's terms

`Router::from_service(CaptivePortal)` makes a `PathRouterService` the router's
*base*, and every `.route(..)` added on top is tried first with that base as the
fallback (`src/routing.rs:1268-1285` shows the same "new item first, existing
router as fallback" shape for `nest_service`). So the catch-all is structurally
incapable of shadowing a real route, which is what keeps it from redirecting the
portal page to itself. It answers:

```
HTTP/1.1 302 Found
Location: http://192.168.4.1/
<a one-line body>
```

See section 4 for why the body is not optional.

### 1.4 Sockets and buffers

| Stack | Sockets | Why |
|---|---|---|
| STA (existing) | frames UDP, control UDP, mDNS UDP, DNS (automatic), DHCP (automatic), **+1 TCP listener** | `StackResources<6>` -> `StackResources<8>` |
| AP (new) | 1 TCP listener, UDP/67 (DHCP), UDP/53 (DNS), 1 spare | `StackResources<4>` |

Buffers, one HTTP worker per stack:

| Buffer | Size | Why |
|---|---|---|
| picoserve `http_buffer` | 2048 | request line + headers + the part of the body already arrived. Our longest request is a `POST /api/v1/wifi` form; 2 KB is generous. |
| TCP rx | 2048 | the receive window during a firmware upload. Smaller throttles the upload; larger buys throughput we do not need. |
| TCP tx | 1024 | the portal page goes out in 1 KB chunks against backpressure. |
| DHCP rx/tx + work | 3 x 640 | a BOOTP packet is 300-576 bytes. |
| DNS rx/tx + work | 3 x 512 | a probe query and its single A answer. |

---

## 2. Soft-AP in `esp-radio 1.0.0-beta.1`

### 2.1 The API, at this exact version

- `Config::AccessPointStation(StationConfig, AccessPointConfig)` exists
  (`src/wifi/mod.rs:376`) and maps to `WIFI_MODE_APSTA` (`mod.rs:3044`).
  `set_config` applies the AP config then the STA config (`mod.rs:3064-3069`).
- `AccessPointConfig` (`src/wifi/ap.rs:36-61`) is a `BuilderLite`:
  `with_ssid`, `with_ssid_hidden`, `with_channel`, `with_authentication`,
  `with_max_connections`, `with_dtim_period`, `with_beacon_timeout`. Defaults
  (`ap.rs:87-100`): SSID `iot-device`, channel 1, `AuthenticationMethodConfig::Open`,
  `max_connections: 255` (clipped to the hardware encryption-key count,
  `mod.rs:3542,3555`), `dtim_period: 2`, `beacon_timeout: 300`.
  `validate()` (`ap.rs:64-84`) rejects WEP and rejects an **empty** password -
  so `Open` is the only way to have no password.
- `Interface::access_point()` / `try_access_point()` (`mod.rs:1584-1599`) is a
  second singleton, guarded by `AP_BIT` beside the station's `STA_BIT`
  (`mod.rs:1523-1534`). Dropping the interface releases the bit
  (`mod.rs:1633-1641`), so a build can bring the AP up and down at runtime.
- `set_config` calls `esp_wifi_stop()` **only when the mode changes**
  (`mod.rs:3049-3051`). STA -> APSTA stops and restarts the radio; APSTA ->
  APSTA with different credentials does not. That is why the state machine below
  goes into APSTA once and stays there for the whole portal episode.
- Station events: `connect_async` (`mod.rs:3337`) resolves `Ok(ConnectedInfo)`
  or `Err(ConnectionError::Failed(DisconnectedInfo))` carrying a
  `DisconnectReason` - which is how `TRIAL` distinguishes a wrong password
  (`FourWayHandshakeTimeout`, `MicFailure`, `_802_1xAuthenticationFailed`) from
  a missing network (`NoApFound` in `DisconnectReason`).
- AP events: `wait_for_access_point_connected_event_async`
  (`mod.rs:3483-3521`) reports a phone associating or leaving. That is the
  signal `RETRY` uses to hold off while someone is on the portal.

### 2.2 Scanning

`scan_async` (`mod.rs:3277-3306`) is the only scan API and its doc says plainly:
**"Scanning is not supported in AcessPoint-only mode"** (`mod.rs:3261`). The
settings page wants a network list, so the device must be in APSTA while the
portal is up. That settles question 2 on function alone.

Two costs to note. `scan_async` returns `alloc::vec::Vec<AccessPointInfo>` -
it allocates, on our 96 KB heap, one ~90-byte record per network found; cap it
with `ScanConfig::with_max(16)` (`scan.rs:104`) and build the JSON straight out
of the `Vec` before dropping it. And scanning takes the radio off the AP's
channel for `min..max` per channel (default 10-20 ms active, `scan.rs:60-66`),
so the phone sitting on the portal sees a ~300 ms stall per scan. The page must
ask for a scan explicitly (a "rescan" button), never on a timer.

### 2.3 The single-PHY trap - this is the important one

ESP-IDF, in the `esp_wifi_set_config()` attention block:

> "ESP devices are limited to only one channel, so when in the soft-AP+station
> mode, the soft-AP will adjust its channel automatically to be the same as the
> channel of the station."
> - <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/network/esp_wifi.html>

and in the Wi-Fi guide:

> "the home channel of AP and station must be the same ... the ESP32 in AP mode
> will notify the connected stations about the channel migration using a Channel
> Switch Announcement (CSA)"
> - <https://docs.espressif.com/projects/esp-idf/en/v5.5/esp32/api-guides/wifi.html>

So the moment `TRIAL` associates with a home network on channel 6, the soft-AP
hops to channel 6 and fires a CSA at the phone standing on it. Phone CSA support
is uneven. **Design consequence:** the "did it work?" answer must survive the
phone briefly losing and re-finding the AP. A `setTimeout(location.href='.')`
full-page reload survives it. A WebSocket, an EventSource or a `fetch()` poll
does not - and section 4 shows that a `fetch()` poll does not work inside the
iOS captive browser anyway. The two constraints point the same way.

### 2.4 RAM cost of the radio itself

`esp-radio`'s own module docs, measured with `esp-alloc`'s internal heap stats
(`src/wifi/mod.rs:15-27`):

> * Station: 47 - 57k
> * Open Access Point: 53 - 63k

These are heap, not `.bss`, and they are alternatives rather than a sum - but
APSTA runs both control blocks, and the firmware's telemetry already reports
~45 KB of a 96 KB heap in use in station mode. **An APSTA build's heap headroom
is the second unknown of this card and the first thing the build should print.**
`ControllerConfig`'s `rx_queue_size` is applied to `DATA_QUEUE_RX_AP` and
`DATA_QUEUE_RX_STA` separately (`mod.rs:2734-2735`), so APSTA doubles that queue
- our `with_rx_queue_size(3)` (`firmware/src/main.rs:568`) means 3 frames each,
not 3 shared. Good for the frame path (AP traffic cannot evict frame packets),
a cost in RAM.

---

## 3. DHCP and the DNS catch-all

**`edge-dhcp 0.8.0`.** Its `io::server::run(server, options, socket, buf)` takes
anything that is `UdpReceive + UdpSend` (`src/io.rs:49-57`), and its own doc
block is explicit:

> "The server works with regular UDP sockets (not requiring raw socket or MAC
> address control) ... Broadcast messages work correctly ... This covers most
> common DHCP scenarios (DISCOVER/OFFER/REQUEST/ACK)"
> - `edge-dhcp-0.8.0/src/io.rs:33-39`

That is the whole reason this is possible: `edge-nal-embassy 0.9` has `Udp`,
`Tcp` and `Dns` and **no raw socket**, and earlier `edge-dhcp` versions needed
`edge-raw`. `Server::<F, N>::new(now, ip)` defaults the pool to `.50 - .200`
(`src/server.rs:193-201`) and keeps leases in a `heapless::LinearMap<_, _, N>`;
N = 4 is plenty for a provisioning AP. `ServerOptions` (`src/server.rs:24-30`)
carries `gateways`, `subnet`, `dns`, `lease_duration_secs` and `captive_url`.

**`edge-captive 0.8.0`.** `io::run(stack, addr, tx, rx, ip, ttl)` binds UDP/53
and answers every `A`/`IN` question with our address (`src/lib.rs:100-118`);
anything that is not a `QUERY` opcode gets `NOTIMP`, and a non-A question gets
`NOERROR` with no answer - which is the right thing for `AAAA`, because the
client then falls back to A. It is ~200 lines over `domain 0.12.1`, the same
`domain` `edge-mdns 0.8.0` already links, so it adds almost no code. Bind it
explicitly to `0.0.0.0:53`; its `DEFAULT_SOCKET` is the IPv6 one
(`src/io.rs:9`), exactly like `edge-mdns`'s, and `firmware/src/mdns.rs:76-79`
already records why that is a trap.

**DHCP option 114 (RFC 8910): ship it, never rely on it.** `edge-dhcp` already
implements it - `ServerOptions::captive_url` (`src/server.rs:29`), `CAPTIVE_URL:
u8 = 114` (`src/lib.rs:822`) - so it is one line. But:

- RFC 8910 says the URI "MUST be that of the captive portal API endpoint
  ([RFC8908])" and "SHOULD NOT contain an IP address literal"
  (<https://www.rfc-editor.org/rfc/rfc8910.txt> §2). RFC 8908 §4 then requires
  that endpoint be **HTTPS with a certificate valid for the provisioned
  hostname** (<https://www.rfc-editor.org/rfc/rfc8908.txt>). A device at
  `192.168.4.1` can satisfy neither.
- Apple has honoured it since iOS 14 / Big Sur and *prefers* it to the probe
  (Tommy Pauly, IETF capport WG, 2020-06-22,
  <https://mailarchive.ietf.org/arch/msg/captive-portals/QoR4xbHFIgKZxMbCjK-2Muh9JR0/>),
  but Apple's own developer note is explicit that "Your Captive Portal API
  server must be running on a host with TLS encryption"
  (<https://developer.apple.com/news/?id=q78sq5rv>). Android 11+ honours it and
  treats it as authoritative for "portal" but not for "not portal". Windows has
  no documented support. systemd-networkd parses it since v254 and nothing
  consumes it.
- ESP-IDF's own captive-portal example advertises plain
  `http://192.168.4.1` through `ESP_NETIF_CAPTIVEPORTAL_URI`
  (<https://github.com/espressif/esp-idf/blob/master/examples/protocols/http_server/captive_portal/main/main.c>),
  violating both constraints, and field reports of it not firing exist.

So: set it to `http://192.168.4.1/portal`, expect it to do nothing, and make the
DNS + HTTP path carry the whole weight.

---

## 4. Captive-portal detection: what each OS probes, and what we answer

This is the part where guessing costs a day on the bench, so the sources are
primary and quoted.

### 4.1 The probes

| OS | Probe | Expected "no portal" answer |
|---|---|---|
| Apple iOS/macOS | `http://captive.apple.com/hotspot-detect.html` | 200, **69 bytes**: `<HTML><HEAD><TITLE>Success</TITLE></HEAD><BODY>Success</BODY></HTML>\n` (the legacy `www.apple.com/library/test/success.html` is the same 68 bytes **without** the newline). UA `CaptiveNetworkSupport-<version> wispr` - hyphen, not slash. |
| Android | `http://connectivitycheck.gstatic.com/generate_204` + HTTPS `https://www.google.com/generate_204`, fallbacks `http://www.google.com/gen_204`, `http://play.googleapis.com/generate_204` | **204** |
| Windows 10/11 | `http://www.msftconnecttest.com/connecttest.txt`, plus a DNS probe of `dns.msftncsi.com` expecting `131.107.255.255` (IPv6: `fd3e:4f5a:5b81::1`) | 200 whose body is exactly `Microsoft Connect Test` (22 bytes, no newline) |
| NetworkManager | distro-dependent; GNOME's is `http://nmcheck.gnome.org/check_network_status.txt` | header `X-NetworkManager-Status: online`, **or** a body with the `response=` prefix (default `NetworkManager is online`, 24 bytes, no newline); with `response=` empty a 204 or an empty 200 |
| Firefox 155+ | `http://firefox-portal-detection.com/generate_204` (it moved off `detectportal.firefox.com` in ~Aug 2026, bug 2049252) | 200 or 204 whose body is **exactly empty**; it follows **no** redirects (`redirectionLimit = 0`) |

Sources: AOSP `NetworkStack` `res/values/config.xml` and
`NetworkMonitor.java` on `main`
(<https://android.googlesource.com/platform/packages/modules/NetworkStack/+/refs/heads/main/res/values/config.xml>);
Microsoft Learn KB 4494446
(<https://learn.microsoft.com/en-us/troubleshoot/windows-client/networking/internet-explorer-edge-open-connect-corporate-public-network>)
and the NCSI overview
(<https://learn.microsoft.com/en-us/windows-server/networking/ncsi/ncsi-overview>);
`NetworkManager.conf(5)`
(<https://networkmanager.dev/docs/api/latest/NetworkManager.conf.html>);
mozilla-central `modules/libpref/init/all.js` and
<https://firefox-source-docs.mozilla.org/networking/captive_portals.html>;
Apple's enterprise-hosts article <https://support.apple.com/en-us/101555>, which
lists **only** `captive.apple.com` - the wider `appleiphonecell.com` /
`gsp1.apple.com` list quoted everywhere is iOS 6/7-era vendor documentation and
should not be relied on.

### 4.2 What makes each one decide "portal"

- **Apple**: a 302/307 to the portal, *or* a 200 whose body is not the Success
  HTML. Both pop the CNA sheet.
- **Android** (`CaptivePortalProbeResult`, `NetworkMonitor.sendHttpProbe()`):
  `isPortalCode(c) = c != 204 && c >= 200 && c <= 399`. So a 302 is a portal and
  a 200 with a real body is a portal - **but a 200 with no `Content-Length`, or
  with `Content-Length <= 4`, is rewritten to `FAILED_CODE = 599`**, with the
  source comment "There's no point in considering this a captive portal as the
  user cannot sign-in to an empty page". Since Android S a portal with no
  `Location` header gets `redirectUrl` set to the probe URL itself, which is what
  makes a bare "200 + login page" openable. Aggregation: **an HTTP probe saying
  portal beats an HTTPS probe failing**; `PARTIAL_CONNECTIVITY` only happens when
  HTTP said *success* and HTTPS failed. One guard to know about: if the probe URL
  resolves to a **private IP**, Android fails the evaluation rather than
  reporting a portal - and our wildcard DNS returns 192.168.4.1 for everything.
- **Windows**: any 200 whose payload is not `Microsoft Connect Test`, or a
  302/304, is `ActiveHttpProbeFailedHotspotDetected`. Microsoft's own
  portal-authoring guidance: *"don't redirect some requests and drop others.
  Keep redirecting all requests until authentication succeeds."*
  (<https://learn.microsoft.com/en-us/windows-hardware/drivers/mobilebroadband/captive-portals>).
- **Firefox**: anything that is not a 200/204 with an exactly-empty body starts
  the login flow, and it follows no redirects, so the redirect *target* is
  irrelevant to it.

### 4.3 The rule for screeny

**DNS**: answer every A query with 192.168.4.1, `NOERROR` (not `NXDOMAIN`), short
TTL. That is what WLED, Tasmota, tzapu WiFiManager and the ESP-IDF example all do.

**HTTP**: `302 Found` + `Location: http://192.168.4.1/` **with a short non-empty
body**, for every request whose `Host:` header is not one of ours. The body is
not decoration: ESP-IDF's own example carries the comment *"iOS requires content
in the response to detect a captive portal, simply redirecting is not
sufficient."*
(<https://github.com/espressif/esp-idf/blob/master/examples/protocols/http_server/captive_portal/main/main.c>),
and Android classifies a `Content-Length <= 4` response as *failed*, not portal.
302 vs 307 is immaterial - every probe is a GET, 307 only preserves the method -
and everyone in the field uses 302. 511 (RFC 6585) is not used in practice.

**The `Host:` allow-list** is a rule, not a list of probe domains. No well-known
project special-cases `captive.apple.com` or `connectivitycheck.gstatic.com` by
name; they all test the *shape* of the host. tzapu WiFiManager:
`bool doredirect = serverLoc != server->hostHeader(); // redirect if hostheader
not server ip, prevent redirect loops`. Tasmota:
`if (WifiIsInManagerMode() && !ValidIpAddress(Webserver->hostHeader().c_str()))`.
WLED: `if (!isIp(hostH) && hostH.indexOf(F("wled.me")) < 0 && hostH.indexOf(cmDNS) < 0 ...)`.
Ours:

```
redirect unless Host is
    192.168.4.1           (or 192.168.4.1:<port>)
    any bare IPv4 literal (so the LAN address works too)
    screeny-<id>.local    (the mDNS name)
```

**The alternative, which also works**: answer *every* unknown URL with the portal
HTML at 200. That is what ESPHome does, deliberately - *"All other requests get
the captive portal page. This includes OS captive portal detection endpoints
which will trigger the captive portal when they don't receive their expected
responses"* (`esphome/components/captive_portal/captive_portal.cpp`). It
satisfies all four OSes. Its cost is sending the whole page on every probe, and
probes are frequent. **Recommendation: 302 + body**, and keep the 200-with-page
behaviour in mind as the one-line fallback if a device turns out not to follow it.

### 4.4 The mini-browser traps, and why they settle the design

**iOS CNA (the "web sheet")**, per the Wi-Fi Alliance's captive-behavior
reference (<https://captivebehavior.wballiance.com/>):

- JavaScript runs, but `alert()` and `confirm()` do not, there is no
  `localStorage`/`sessionStorage`, cookies are destroyed when it closes, and
  **external stylesheets and web fonts fail**. Inline everything.
- **"A javascript-triggered AJAX call for example will not result in the CPMB
  performing an additional check."** Only a **full-page navigation** makes iOS
  re-probe and flip the sheet's button from Cancel to Done. *This kills any
  "poll `/api/v1/status` with `fetch()`" design inside the portal.*
- Tapping **Cancel** *"disassociate[s] the device from the captive Wi-Fi
  network"* - i.e. it drops our AP mid-provisioning. The **"Without Internet"**
  option keeps the association (<https://support.apple.com/en-us/102554>).
- Switching apps closes the sheet and disconnects - fatal if the user leaves to
  fetch a password from a password manager.
- **File upload: assume it does not work.** No primary Apple source says
  `<input type=file>` is disabled, but the WBA reference says *"Most external
  services (file system, applications and etc.) are not accessible from CPMB"*.
- Flagged, not asserted: an unanswered developer report of captive detection
  regressing on iOS 26 (<https://developer.apple.com/forums/thread/805035>).

**Android captive-portal login** is a WebView
(`packages/modules/CaptivePortalLogin/.../CaptivePortalLoginActivity.java`):
`setJavaScriptEnabled(true)` and `setDomStorageEnabled(true)`, so JS and DOM
storage do work - but there is **no `onShowFileChooser` override**, so
`<input type=file>` does nothing there either. The Android 12+ Custom Tabs path
is opt-in through an RFC 8908 JSON key, which we cannot serve (section 3), so we
get the WebView.

**Consequences, which the build cards must honour:**

1. The credentials form is a plain `<form method=post>` with a **full-page**
   result, and the "did it work?" page refreshes itself with
   `setTimeout(() => location.href = '.', 2000)`. No `fetch()`, no WebSocket, no
   EventSource on the portal page. (The `/api/v1/status` JSON endpoint still
   exists - it is for the Studio and for a real browser, not for the CNA.)
2. The **firmware upload lives on the LAN page only**, never on the portal page.
   The portal's firmware section is one line of text: "open
   `http://192.168.4.1` in Safari or Chrome to update firmware". The card-200
   sink does not change; only where the form is offered.
3. Everything inline: no external CSS, no web fonts, no CDN. The page in the
   spike (`firmware/src/web_spike_portal.html`) is already written that way.

---

## 5. The state machine

### 5.1 How other firmwares answer "did my credentials work?"

Four patterns; only two actually answer the question.

| Pattern | Who | Does the user learn the result? |
|---|---|---|
| A. Keep AP+STA up, trial the credentials, report on a self-reloading page | **Tasmota** | **yes** |
| B. Keep AP+STA up, client polls a status endpoint | ESP-IDF `wifi_provisioning`, Shelly Gen2 | yes (but see 4.4 - not inside the iOS CNA) |
| C. An out-of-band channel that survives the radio switch | Improv (WLED, Tasmota, ESPHome, ESP Web Tools) | yes |
| D. "Saved, we'll try - go reconnect to your home WiFi and find us" | **WLED**, **ESPHome**, **tzapu WiFiManager** | **no** |

Tasmota is the model. `HandleWifiConfiguration()` on save does **not** commit and
does **not** reboot: it sets `Wifi.wifiTest = WIFI_TESTING`, clears
`restart_flag`, calls `WiFiHelper::begin(ssid, pass)`, and serves a page saying
"Trying to connect device to network" with
`setTimeout(function(){location.href='.';}, 10000)`. A per-second tick resolves
it into **"Successful WiFi Connection"** plus a link to the new IP, or
**"Connect failed to \<ssid\> / Please, check your credentials"**. Its manager
window is `WIFI_CONFIG_SEC = 180`.
(<https://github.com/arendst/Tasmota/blob/development/tasmota/tasmota_xdrv_driver/xdrv_01_9_webserver.ino>)

ESP-IDF's `wifi_provisioning` has the best *vocabulary* to copy - its
`scheme_softap.c` sets `WIFI_MODE_APSTA` explicitly so the AP survives the trial,
its status enum is `Connected / Connecting / Disconnected / ConnectionFailed`
with a reason of `AuthError` or `NetworkNotFound`, and the success message
carries `ip4_addr` so the phone learns the new LAN address **before** the AP goes
away. `CONFIG_WIFI_PROV_AUTOSTOP_TIMEOUT` defaults to 30 s after success.

WLED is the anti-pattern and worth recording: `initConnection()` does
`WiFi.mode(WIFI_MODE_NULL); apActive = false;` - **the phone's AP vanishes the
instant you save** - and the documented recovery is *"Reconnect your phone or
computer to your home WiFi network"* and then mDNS or the router's device list.
Which brings up the trap that makes that advice fail: **Chrome on Android does
not resolve `.local`** (it reimplements its own resolver and skips mDNS).

**screeny has something none of these projects have: a 64x32 panel.** Putting
the state and then the acquired IP address on the panel is the one channel that
cannot be lost when the radio switches. That is the answer to "how does the user
find the device afterwards", and it costs nothing - `screens.rs` already draws
an IP address.

### 5.2 The proposed machine

```
                 ┌──────────┐
   boot ────────▶│  BOOT    │ store has credentials?
                 └────┬─────┘
             yes ┌────┴────┐ no
                 ▼         ▼
           ┌──────────┐  ┌──────────────────────────────┐
           │ JOINING  │  │           PORTAL             │
           │ 3 tries  │  │ APSTA up, AP `screeny-<id>`  │
           │ ~45 s    │  │ DHCP + DNS + HTTP on         │
           └──┬────┬──┘  │ 192.168.4.1, QR on the panel │
       ok     │    │fail └───┬──────────────────────▲───┘
              │    └─────────┘  credentials posted  │  trial failed
              ▼                        │            │  (AP never dropped)
        ┌───────────┐                  ▼            │
        │  ONLINE   │            ┌───────────┐      │
        │ AP down   │◀───────────│   TRIAL   │──────┘
        │ LAN HTTP  │  trial ok  │ APSTA up  │
        │ mDNS up   │  (+30 s    │ 3 tries   │
        └─────┬─────┘   grace)   └───────────┘
              │ link down > 60 s        ▲
              └─────────────────────────┘ RETRY tick, no AP client
```

| From | Event | To | Side effects |
|---|---|---|---|
| `BOOT` | store has credentials | `JOINING` | `WIFI_STATE = CONNECTING`, panel shows "joining wifi" (already drawn) |
| `BOOT` | store empty | `PORTAL` | - |
| `JOINING` | joined | `ONLINE` | `WIFI_STATE = CONNECTED`, mDNS announce, LAN HTTP up |
| `JOINING` | 3 attempts failed (~45 s) | `PORTAL` | `WIFI_STATE = FAILED` |
| `PORTAL` | `POST /api/v1/wifi` | `TRIAL` | reply **first** (spec 8.2), *do not commit*, AP stays up |
| `TRIAL` | joined | `ONLINE` | commit to the store, hold the AP a further **30 s** so the page can report the new IP, then drop it |
| `TRIAL` | failed | `PORTAL` | report `AuthError` / `NetworkNotFound` on the next page load; nothing written to the store |
| `PORTAL` | `RETRY` tick (10 min) **and** no station associated to the AP | `JOINING` | so a network that came back at 3 a.m. is found without anyone touching the device |
| `ONLINE` | link down for > 60 s | `JOINING` | the existing `wifi_task` reconnect loop, unchanged |
| `ONLINE` | `JOINING` fails 3 times | `PORTAL` | the device says why on the panel rather than sulking |
| any | button (card 202) | `PORTAL` | card 202 owns the gesture; this is the only thing it needs from here |

Mapping onto the spec:

- Telemetry byte 46 (`state`) reads **`PROVISIONING` (4)** in `PORTAL` and
  `TRIAL`. It is an *overlay*, exactly as spec section 7.3 says: frame handling
  continues underneath, so a sender on the LAN that is still streaming keeps
  streaming while the device is in portal mode on the AP side.
- `GET_WIFI`'s state byte: `CONNECTING` in `JOINING` and `TRIAL`, `CONNECTED` in
  `ONLINE`, `FAILED` after a failed trial, `DISCONNECTED` in `PORTAL` with an
  empty store.
- Spec section 8.2's "reply **before** disconnecting" applies verbatim to
  `POST /api/v1/wifi` as well as to `SET_WIFI`, and is easier here because
  APSTA never disconnects the AP at all.
- **Spec section 8.1 (the serial console) should be struck**, as the card says,
  and section 8.3's fallback rule rewritten: "try stored credentials, then the
  compile-time ones, then display the failure" becomes "try stored credentials,
  then the compile-time ones, then **raise the portal** and display the SSID and
  the QR". The compile-time credentials stay as step 2 - they are what makes a
  bench flash come straight up.

The one timing number worth arguing about is the `PORTAL` -> `JOINING` retry
interval. 10 minutes is a guess informed by WLED's 5-minute throttle and
Tasmota's 3-minute manager window; each retry costs the phone on the portal a
~45 s outage, which is why it is gated on no AP client being associated.

---

## 6. The RAM budget - the real risk

Everything below is measured in this worktree, release profile, `lto = "fat"`,
`xtensa-esp32-elf-size -A`, on the `esp` toolchain.

| config | `.text` | `.rodata` | `.data` | `.bss` | `.stack` (the remainder) |
|---|---|---|---|---|---|
| `main` (baseline) | 531205 | 73064 | 31492 | 127040 | **37536** |
| + AP interface and its stack | 533117 | 73264 | 31492 | 131008 | 33568 |
| + AP + picoserve | 602917 | 82872 | 31892 | 138640 | 25536 |
| + AP + DHCP/DNS | 549389 | 74952 | 31708 | 136424 | 27928 |
| + QR encoder only | 541693 | 75056 | 31556 | 133240 | 31272 |
| **everything** | **629269** | **86408** | **32172** | **150200** | **13688** |

Flash image (`espflash save-image --chip esp32`): **743,408 -> 855,488 bytes**,
+112,080 (+15.1%). 20.7% of a 4 MB app slot, so card 200's two-slot table has
plenty of room.

Attribution, from the deltas:

| piece | flash | `.bss` |
|---|---|---|
| AP interface + second `embassy-net` stack (`StackResources<4>`) | ~2.1 KB | ~3.9 KB |
| picoserve + its buffers (2 KB http, 2 KB rx, 1 KB tx) + serde JSON | ~78 KB | ~7.6 KB |
| `edge-dhcp` + `edge-captive` + their UDP buffers | ~18 KB | ~5.4 KB |
| `qrcodegen-no-heap` (+ a spike-only 6 KB `Frame`) | ~10.5 KB | ~0.06 KB + 6 KB scaffold |

**The trap the card warned about is real and it bites here.** `.stack` is not a
configured size: it is whatever is left between `_bss_end` and `0x3ffe0000`, and
`firmware/src/main.rs:424` already records the symptom ("a *write to the stack
guard value on ProCpu* panic inside `main`"). Two `FrameBuffer::new()` temporaries
of 12 KB each are materialised on that stack at `main.rs:501-502`. **13.4 KB will
not survive that.** The spike is compile-only so it was never going to run, but
the build must fix this before it flashes anything. Three levers, cheapest first:

1. **Drop the spike's 6 KB `Frame`.** The portal screen draws into the existing
   triple buffer like every other screen; it needs no buffer of its own. That
   alone is 6 KB back, `.stack` -> ~19.7 KB.
2. **Move the framebuffer construction off the stack.** `FrameBuffer::new()`
   into a `ConstStaticCell` (or construct in place) removes the 24 KB
   *transient* requirement entirely, which is what the stack is actually sized
   for. This is the right fix and it is small.
3. **Shrink the heap by another 8-16 KB** (`main.rs:426`'s second
   `heap_allocator!`), which is how card 008 paid for its `.bss` growth. But the
   heap is where `esp-radio`'s APSTA control blocks and `scan_async`'s `Vec`
   live, and telemetry already reports ~45 KB of 96 KB in use in *station*
   mode. **Do not take this lever until an APSTA build has printed its real
   `esp_alloc::HEAP.stats()`.** That measurement is the first task of the first
   build card.

Buffers that can be cut if it comes to it: TCP rx 2048 -> 1460 (one MSS) costs
upload throughput only; the AP's `StackResources<4>` -> `<3>` if the spare is
not wanted; DNS 512 -> 320 (a probe query and one A answer fit easily).

And one cheap win: the portal page is 1.6 KB of HTML in `.rodata` today.
Gzipping it at build time and serving `Content-Encoding: gzip` would roughly
halve that - but 800 bytes is not worth a build step, and the iOS CNA's
restrictions mean the page will never grow large. **Serve it plain.**

---

## 7. The page and the API

One self-contained HTML page, no external assets, inline CSS and inline JS, at
`GET /`. Sections: **status** (the `GET_INFO` + telemetry numbers), **network**
(scan list, SSID, password), **firmware** (LAN page only, see 4.4), **reboot**.
`picoserve::response::File::html` gives it a compile-time SHA-1 ETag and a 304
on revisit for free (`src/response/fs.rs:99-106`).

Underneath, a JSON API the Studio can also use (card 106 owns devices and
state):

| Route | Body | Reply |
|---|---|---|
| `GET /api/v1/status` | - | `{id, name, fw, uptime_ms, heap_used, heap_size, rssi_dbm, brightness, idle_mode, wifi_state, ssid, ip, state, portal, slot}`. `ssid` is the SSID; **there is no psk field, ever** |
| `GET /api/v1/telemetry` | - | the 48 bytes of spec 6.7, as named JSON fields, so a browser and a UDP sender see the same numbers |
| `GET /api/v1/networks` | - | `{networks:[{ssid, rssi, secure}]}` - triggers a scan, rate-limited to one per 10 s |
| `POST /api/v1/wifi` | urlencoded `ssid=&psk=` | `{result:"trying"}`, sent **before** the radio work (spec 8.2) |
| `GET /api/v1/wifi` | - | `{state, ssid, ip, reason}` where `reason` is `null` / `"auth"` / `"not_found"` - this is what the portal page's full-page reload reads |
| `POST /api/v1/settings` | `{name?, brightness?, idle_mode?}` | the applied values, same clamping as `SET_BRIGHTNESS` |
| `POST /api/v1/firmware` | raw `application/octet-stream`, `Content-Length` set | streamed to card 200's sink; `{ok, written}` or a 400 |
| `POST /api/v1/reboot` | `{confirm:"RBOO"}` | sent before rebooting, like `REBOOT` |
| `POST /api/v1/identify` | `{duration_ms}` | mirrors `IDENTIFY` |

Notes:

- **Raw body, not `multipart/form-data`,** for the firmware upload. picoserve
  parses `content-length` only (`src/request.rs:995-998`) and has no multipart
  parser; a browser `fetch('/api/v1/firmware', {method:'POST', body: file})`
  sends the raw bytes with a `Content-Length`, which is exactly what the
  streaming reader wants.
- **Auth: open, same posture as spec 8.4** (owner's decision). Design so a PIN
  can be added: every mutating route takes an optional `X-Screeny-Pin` header
  and a `counter`, checked in one function that today returns `Ok(())`. Card 041
  owns the real thing.
- **mDNS: advertise `_http._tcp` too**, with the same instance name, so the
  Studio and a Bonjour browser find the page without knowing the port.
  `edge-mdns`'s `Service` is a single struct; `firmware/src/mdns.rs:140` builds
  one and would build two. The TXT record stays the `GET_INFO` bytes, unchanged.
- **The PSK invariant of spec 8.4 holds over HTTP**: never in a reply, never in
  a log line (log its *length* if anything), never on the panel. The spike's
  `post_wifi` logs `psk_len` and the compiler-checked `Status` struct has no
  field for it.

---

## 8. Host-testability

Workers and CI have no device, so as much as possible should be a `no_std`
module under `crates/` that a host test drives.

**What can and should move to `crates/`:**

| thing | where | why |
|---|---|---|
| the provisioning state machine | a new `crates/provision`, `no_std`, no alloc | a pure `fn step(state, event, now_ms) -> (state, actions)`. Every transition in 5.2, including the 3 a.m. retry, becomes a unit test with no radio. This is the single highest-value extraction. |
| the portal screen renderer | `crates/provision`, drawing into `screeny_proto::Rgb888Frame` | the same function the firmware calls and `lab/src/bin/portal-mock.rs` renders; the QR layout stops being two implementations |
| the WIFI: URI builder and its escaping | `crates/provision` | the escaping rules in section 9 are exactly the kind of thing that is wrong once and never noticed |
| the JSON shapes of section 7 | `crates/proto` | "one implementation of each thing": the Studio deserialises what the firmware serialises, so the structs belong beside the wire format, behind a `serde` feature |

**What must stay in the firmware:** the picoserve router (it is glue), the
`esp-radio` config calls, and the socket plumbing.

**Should `crates/sim` serve the same HTTP API?** Yes, and it is nearly free once
the JSON shapes are in `crates/proto`: the simulator already models `GET_INFO`,
telemetry and `SET_WIFI`, and parked card 081 already asks it to model the join
states and the `PROVISIONING` overlay. A sim that also answers
`GET /api/v1/status` on a host port lets the Studio's device page be built and
tested with no hardware at all. That is a build card, and it should depend on
081 being unparked.

---

## 9. The portal screen

### 9.1 The QR

The owner measured on 2026-09-19: `WIFI:T:nopass;S:screeny-4a00a4;;` is **exactly
32 bytes**, lands on **version 2-L** (25x25 modules), and at one LED per module
with **standard polarity** (quiet zone and light modules lit white, dark modules
off) and a **3-pixel lit quiet zone**, centred on the 64x32 panel at default
brightness, it scanned easily from a phone. `lab/src/bin/portal-mock.rs`
reproduces the encode exactly, with the same `qrcodegen-no-heap 1.8.1` the
firmware links.

**The naming rule, measured** (`cargo run --release --bin portal-mock`):

| SSID length | `WIFI:T:nopass;S:<ssid>;;` | `WIFI:S:<ssid>;;` |
|---|---|---|
| <= 14 | 32 bytes, **version 2** | 23 bytes, version 2 |
| 15 | 33 bytes, version 3 | 24 bytes, version 2 |
| 23 | 41 bytes, version 3 | 32 bytes, **version 2** |
| 24 | 42 bytes, version 3 | 33 bytes, version 3 |

So: **the AP SSID must be at most 14 characters**, and `screeny-<6 hex>` is
exactly 14. That is not comfortable - it means the AP SSID can never be derived
from a `SET_NAME` friendly name, and a future `screeny-setup-<id>` would not
fit. The escape hatch is the **short open form** `WIFI:S:screeny-4a00a4;;`,
which the Wi-Fi Alliance WPA3 spec v3.5 §7.1 asks for on an unauthenticated
network (`T` absent means "unauthenticated") and which ZXing's parser - the one
Android's `WifiQrCode.java` implements - defaults to `nopass` when `T` is
missing. It is nine bytes shorter and buys nine characters of SSID at the same
QR version. It is *not yet measured on the owner's phone*, so:

- **ship `T:nopass`** (measured, works, 14-character limit), and
- put one bench test in a build card: show the short form on the panel and scan
  it with an iPhone and an Android phone. If it works, switch, and the SSID
  limit becomes 23.

The rule if the payload ever needs version 3 (29x29): with a 1-pixel quiet zone
the block is 31x31 of a 32-row panel, so **there is no room for text beside it**
and the screen becomes layout B (QR alone, centred), with the SSID shown on an
alternating second screen. Do not go past version 3: version 4 is 33x33 and does
not fit at all.

Escaping, for the builder in `crates/provision`: the two specs disagree and
there is no portable encoding for a special character. ZXing (and Android)
backslash-escape `\ ; , : "`
(<https://github.com/zxing/zxing/wiki/Barcode-Contents>); the WFA WPA3 spec
percent-encodes and Android does not understand that. **Constrain the SSID to
`A-Za-z0-9-` and the question never arises** - which `screeny-<id>` already does.

### 9.2 The layouts

Rendered by `lab/src/bin/portal-mock.rs` into `docs/research/img/`:

**Layout A - the one to build** (`201-portal-a-qr-and-name.png`). Version 2-L QR
at x=3..27 with its 3-pixel quiet zone occupying columns 0..30, and 32 columns
left for text: eight `FONT_4X6` characters per line.

```
+----------------------------------------------------------------+
|###############################                                 |
|###############################                                 |
|###############################          +                      |
|###       ##    ### #       ###  ++  +  +++     + + ++          |   "set up"
|### ##### #   ## #  # ##### ### ++  + +  +      + + + +         |
|### #   # ## ## # ### #   # ###   + ++   +      + + ++          |
|### #   # # ###     # #   # ### ++   ++   +      ++ +           |
|### #   # ###  ## # # #   # ###                     +           |
|### ##### # ###  # ## ##### ###                                 |
|###       # # # # # #       ###                                 |
|############  # ###############   +      +                      |   "join"
|###     #   ####  #  # # # ####      +      ++                  |
|#####   ## ##   #### # #### ###   + + + ++  + +                 |
|### #  #   #### #   #       ###   + + +  +  + +                 |
|### #### #    # # ###   ##  ###   +  +  +++ + +                 |
|### ### #  # ###      #  ###### ++                              |
|###      ## ##  ##  ## #  # ###                                 |
|### #  ## # #####   # # ##  ###  ##  ## # #  #   #  ##  # #     |   "screeny-"
|### ##   ##   # # ##     # #### ##  #   ##  # # # # # # # # ### |
|### ## #   #  #   #     #   ###   # #   #   ##  ##  # #  ##     |
|###########    # ## ###     ### ##   ## #    ##  ## # #   #     |
|###       # ###   # # #     ###                         ##      |
|### ##### ### ####  ### ### ### # #      #   #      # #         |   "4a00a4"
|### #   # #  ###        ### ### # #  ## # # # #  ## # #         |
|### #   # # #     #   # ####### ### # # ### ### # # ###         |
|### #   # #  ##### # # # ## ###   # # # # # # # # #   #         |
|### ##### # ##### # # #  ## ###   #  ##  #   #   ##   #         |
|###       # ##   ### ## #   ###                                 |
|###############################                                 |
|###############################                                 |
|###############################                                 |
|                                                                |
+----------------------------------------------------------------+
```

**Layout B** (`201-portal-b-qr-only.png`): version 3, 1-pixel quiet zone,
centred, no text. The fallback if the SSID ever grows.

**Layout C** (`201-portal-c-text-only.png`): no QR at all - "set up wifi" /
"join network" / the SSID / `192.168.4.1`. This is not only a fallback for a
name a QR cannot carry; it is what a user whose phone will not scan needs, so
**the screen should alternate A and C every ~4 s**. The ambient sweep of
`screens.rs` stays, as proof of life.

The panel is also the answer to the post-provisioning problem of 5.1: once
`TRIAL` succeeds, the panel shows the acquired **IP address** - the channel that
cannot be lost when the radio switches, and the thing Chrome-on-Android's lack
of mDNS makes necessary.

---

## 10. Does an HTTP request cost a frame?

The card asks how I know, so: here is the mechanism, and here is the measurement
that must settle it.

**Mechanism.** Core 0 runs one `esp-rtos` embassy executor, cooperatively:
`wifi`, `net` (the embassy-net runner), `frames`, `control`, `mdns`, `telemetry`,
and now `http`. There are no priorities between them. So yes, an HTTP request
shares core 0 with the frame path. What bounds the damage:

- **The display cannot stall.** It is on core 1, its DMA is circular
  (`circular-dma`), and its refresh ISR is in IRAM (`iram`). A late decode shows
  a stale frame for one frame time; it never blanks the panel.
- **The driver queues are separate.** `DATA_QUEUE_RX_AP` and `DATA_QUEUE_RX_STA`
  each get `rx_queue_size` (`esp-radio/src/wifi/mod.rs:2734-2735`), so portal
  traffic on the AP cannot evict frame packets queued on the STA.
- **The frame budget is 33 ms and decode is single-digit milliseconds**
  (telemetry `decode_us`). picoserve awaits on every socket read and write, so
  the longest non-yielding stretch in a request is a header parse over a 2 KB
  buffer.
- **The portal is not up while a sender is streaming.** `PORTAL` means the
  device is not on the LAN. Only the *LAN* HTTP server coexists with a stream,
  and that is one worker with `keep_connection_alive` and a 2 s persistent
  timeout.

**The escape hatch, if it turns out to matter.** `esp-rtos 0.4.0` has
`InterruptExecutor<SWI>` started at a chosen `Priority`
(`src/embassy/mod.rs:317,392`), and software interrupts 2 and 3 are free -
`esp_rtos::start` takes `FROM_CPU_INTR0` and `start_second_core` takes
`FROM_CPU_INTR1` (`firmware/src/main.rs:429,529`). Moving `frames_task` (and the
net runner it depends on) to a higher-priority interrupt executor is possible
without changing any other design decision. **Do not do it speculatively.**

**The measurement that settles it** (an acceptance criterion for the build
card): with a 30 fps stream running to the LAN address, hammer
`GET /api/v1/status` and `GET /` at ~20 req/s for 60 s and require
`frames_shown` within 1% of `frames_rx`, `jitter_us` inside the existing
budget, and `frames_dropped_superseded` unchanged. That is `screeny stats`
against a `wrk`-shaped loop; it is a ten-minute bench run, once, on the final
build.

---

## 11. The spike

`firmware/src/web_spike.rs` and `firmware/src/web_spike/{ap,http,portal,qr}.rs`,
behind `spike-ap` / `spike-http` / `spike-portal` / `spike-qr` (umbrella
`device-web-spike`). **Never flashed.** It exists to make the compiler agree with
every API claim above and to produce the numbers in section 6. It builds clean on
the `esp` toolchain, release, LTO fat:

```
. ~/export-esp.sh && cd firmware && cargo build --release --features device-web-spike
```

It proves, in order: the AP interface and a second statically-addressed
`embassy-net` stack; picoserve routing, a static page, a urlencoded form, JSON
out, a 302 catch-all and a streaming body with a raised timeout; `edge-dhcp` over
a plain UDP socket with option 114; `edge-captive` on UDP/53; and
`qrcodegen-no-heap` rendering a version 2-L code into a `Frame`. The build cards
replace it wholesale - it is evidence, not the implementation.

`lab/src/bin/portal-mock.rs` is the host half: the layouts and the QR-version
table.

---

## 12. Proposed build cards

Not written as card files, as the card instructs. Suggested numbers are from my
range where they are mine to give; the orchestrator owns the real numbering.

**A. Raise the TCP and RAM headroom, and prove the APSTA heap.** Before any
feature: add `"tcp"` explicitly to `embassy-net`'s features, raise the STA stack
to `StackResources<8>`, move the two `FrameBuffer::new()` temporaries off core
0's main stack into `ConstStaticCell`s, and flash a build that brings up APSTA
with an open AP and *nothing else* - no HTTP, no DHCP, no DNS - and logs
`esp_alloc::HEAP.stats()` and the `.stack` remainder for ten minutes with a
stream running. This card exists because section 6 says the RAM budget is the
real risk and the APSTA heap figure is the one number nobody has. Everything
after it is gated on the answer.

**B. `crates/provision`: the state machine, host-tested.** A `no_std`, no-alloc
crate with the state machine of 5.2 as a pure function of (state, event, now),
the `WIFI:` URI builder with its escaping, and the portal-screen renderer that
draws into a `screeny_proto::Rgb888Frame`. Every transition in the table gets a
test, including "portal for an hour, no AP client, retries and recovers" and
"trial fails with `AuthError` and the store is untouched". `lab/src/bin/portal-mock.rs`
switches to calling the crate so the layout stops existing twice.

**C. The soft-AP, DHCP and the DNS catch-all.** Bring up the AP interface, its
`embassy-net` stack at 192.168.4.1/24, `edge-dhcp` on UDP/67 with option 114 and
`edge-captive` on UDP/53, driven by the state machine from B. Acceptance: a phone
associates, gets a lease in the `.50-.200` range, and every DNS name it asks for
resolves to 192.168.4.1. No HTTP yet - this card is done when `ping 192.168.4.1`
works and `dig anything @192.168.4.1` answers.

**D. picoserve and the JSON API.** The router, `GET /api/v1/status`,
`/telemetry`, `/networks`, `GET`/`POST /api/v1/wifi`, `/settings`, `/reboot`,
`/identify`, the 302 catch-all with its `Host:` rule and its non-empty body, and
the optional-PIN check function that returns `Ok(())` today. Shapes live in
`crates/proto` behind a `serde` feature. Acceptance: `curl` against the device on
the LAN and on the AP; the PSK appears in no reply and no log line.

**E. The one page.** The self-contained HTML of section 7, inline everything, no
`fetch()` on the provisioning path - a plain form post and a `setTimeout`
full-page reload, per 4.4 - and the firmware-upload form present only when the
request did not arrive on the AP interface. Acceptance: the CNA sheet opens on
iOS, the sign-in notification appears on Android, and the flow completes on both
without ever leaving the mini-browser.

**F. The portal screen.** Layout A and layout C alternating every ~4 s, drawn
through `crates/provision`, with the acquired IP shown for 60 s after a
successful trial. Acceptance is the owner's eye and a phone: scan the panel, join
the AP.

**G. `crates/sim` serves the same HTTP API.** Depends on unparking card 081. The
simulator answers `GET /api/v1/status` and friends on a host port with the same
`crates/proto` shapes, and models the join states so the Studio's device page can
be developed with no hardware.

**H. Spec surgery.** Strike section 8.1 (the serial console) and rewrite 8.3's
fallback rule to end at the portal rather than at a message. Add the HTTP API as
a new section, and record the `PROVISIONING` overlay's two sub-states. Say in the
commit which side was the bug, per CLAUDE.md.

**I. Bench: does HTTP cost a frame?** The measurement of section 10, once, on
the final build. And the one QR experiment: show `WIFI:S:screeny-4a00a4;;` on the
panel and scan it with an iPhone and an Android phone; if both read it, the SSID
limit goes from 14 to 23 characters and card F's naming rule relaxes.

---

## 13. Open questions for the owner

1. **The AP SSID can be at most 14 characters** with the measured QR form.
   `screeny-4a00a4` is exactly 14, which means the AP name can never be derived
   from a `SET_NAME` friendly name and a `screeny-setup-<id>` would not fit. Is
   `screeny-<id>` the permanent AP name? (Card I's experiment may relax this to
   23; if it does, the question goes away.)
2. **AP security is decided (open)**, so this is recorded for the file only: a
   WPA2 AP with a per-device passphrase would cost a `P:<8+ chars>` field in the
   QR - 11+ more bytes, pushing the payload to version 3 and off layout A - plus
   somewhere for the passphrase to live and be shown. The gain would be that the
   home PSK stops crossing an open network in clear. Spec 8.4 already records
   that the owner does not treat it as a secret.
3. **HTTP auth is decided (open on the LAN)**, so likewise for the file: the
   design leaves an `X-Screeny-Pin` header and a counter on every mutating route,
   checked in one function that returns `Ok(())` today. Card 041 turns it on.
4. **Does the portal have a time limit?** Tasmota's manager window is 3 minutes,
   ESPHome's `ap_timeout` is 90 s, WLED's temporary AP is 5 minutes. I have
   proposed **no limit** - the AP stays up until credentials work - on the
   grounds that a device that cannot reach the network is useless anyway and an
   AP that disappears while you are typing is worse than one that lingers. The
   10-minute background retry means it heals itself. Confirm.
5. **Should the LAN web server also be reachable while a sender is streaming?**
   I have assumed yes (one worker, short keep-alive), because "show me the device
   health page" is the point. If the owner would rather the panel be untouchable
   during a stream, the LAN server can refuse with 503 while `state == LIVE` and
   the frame path stops sharing core 0 entirely.
6. **`_http._tcp` in mDNS**: I recommend advertising it. It means the device
   shows up in Safari's Bonjour list and in any network browser, which is a small
   privacy surface on a home LAN and a real convenience. Confirm.
