# screeny-device-api - the device's HTTP API, defined once

`no_std`, no alloc, no float, no clock, no I/O. The firmware serves this (card 222),
the simulator serves the same thing on a host port (card 224), and the Studio reads it.
Three programs, one set of shapes, so a field the firmware grows is a field the Studio's
parser gets in the same commit or the build breaks. This crate serves nothing and talks
to nothing.

Card 226. The design is `docs/design/device-web.md` (decisions 2, 3, 7) and
`docs/research/007-device-web-and-portal.md` sections 4.4 and 7.

## The route table

Sizes are bytes of JSON, and the size columns come from the constants in the code
(`route::ROUTES`), not from this table - `tests/sizes.rs` keeps them honest.

| Method | Path | Request | Reply | req ≤ | reply ≤ |
|---|---|---|---|---|---|
| GET | `/api/v1/status` | - | `StatusReply` | - | 1018 |
| GET | `/api/v1/telemetry` | - | `TelemetryReply` | - | 426 |
| GET | `/api/v1/networks` | - | `NetworksReply` (≤ 16, strongest first) | - | 3710 |
| GET | `/api/v1/wifi` | - | `WifiReply` | - | 345 |
| POST | `/api/v1/wifi` | urlencoded `WifiForm` | `AcceptedReply` (`"trying"`) | 384 | 24 |
| POST | `/api/v1/settings` | `SettingsRequest` | `SettingsReply` | 373 | 247 |
| POST | `/api/v1/firmware` | raw `application/octet-stream`, streamed | `FirmwareReply` | streamed | 57 |
| POST | `/api/v1/reboot` | `RebootRequest` (`{"confirm":"RBOO"}`) | `AcceptedReply` (`"rebooting"`) | 188 | 24 |
| POST | `/api/v1/identify` | `IdentifyRequest` | `AcceptedReply` (`"identifying"`) | 152 | 24 |

Anything that fails answers `ErrorReply`: `{"error":"<code>"}` with an optional
`"detail"`, and the HTTP status is `ErrorCode::status()` - one function, so the browser's
status and the Studio's code cannot disagree. `ErrorCode::ALL` is the closed set:
`bad_request` `bad_form` `bad_json` `out_of_range` (400), `unauthorized` (401),
`forbidden` (403), `not_found` (404), `method_not_allowed` (405), `busy` (409),
`payload_too_large` (413), `rate_limited` (429), `storage` `wifi` `internal` (500),
`unavailable` (503).

## The examples

`tests/golden/` holds one file per request and reply. **They are the documentation for
the Studio side**: each one is exactly what `serde_json` writes for that value and
exactly what both parsers read back (`tests/golden.rs`). They are pretty-printed so that
they are worth reading; the compact form that goes on the wire is checked in the same
test. `wifi_form.txt` is the urlencoded body, not JSON, because `POST /api/v1/wifi` is a
form post.

```
status.json          status_portal.json      telemetry.json     networks.json
wifi_connected.json  wifi_disconnected.json  wifi_failed.json   wifi_trying.json
settings_request.json    settings_request_brightness_only.json  settings_reply.json
firmware_ok.json     firmware_failed.json
accepted_trying.json accepted_rebooting.json accepted_identifying.json
reboot_request.json  identify_request.json   identify_request_with_pin.json
error.json           error_with_detail.json  wifi_form.txt
```

## What the numbers are for

Every request and reply type has a `MAX_JSON_LEN`, and `tests/sizes.rs` proves each one
by serialising the longest value that type can hold and asserting **equality**. They are
not the same kind of number on each side:

* **Requests.** picoserve's `Json` and `Form` extractors call `read_all()`, which needs
  the whole body contiguous in the HTTP buffer. `route::MAX_REQUEST_LEN` (384, the WiFi
  form) is therefore a real dimension of card 222's server.
* **Replies.** Not a buffer size. picoserve measures a JSON reply by serialising it into
  a counting writer (`Content::content_length`) and then streams it, so no reply needs a
  buffer at all. The reply bound is for `serde-json-core`'s `to_slice` into a fixed
  array, and for the honest answer to "how big can this get".

The bounds assume every byte of every name escapes to six characters (`\u001f`), which
never happens: a real status reply is 364 bytes against a bound of 1018.

## Three things that will bite whoever wires this up

1. **`serde_json_core::from_slice` does not unescape strings, and does not say so.**
   It hands the visitor the raw escaped text, so a name posted with a `\u00e9` in it is
   stored with those six characters in it. Use `from_slice_escaped(body, &mut buf)`.
   The buffer holds one *unescaped* string at a time, so it must be
   `request::MIN_UNESCAPE_BUFFER` (32 bytes, the longest name) or more; picoserve's
   default `Json` extractor is `JsonWithUnescapeBufferSize<T, 32>`, which is exactly
   enough and not a byte more.
2. **picoserve's `Form` extractor cannot parse `POST /api/v1/wifi`.** It rejects a body
   that is not UTF-8, and an 802.11 SSID is a byte string. `form::parse_wifi_form` takes
   the raw body and hands back bytes. It also refuses a duplicate key rather than taking
   the last one: `psk=right&psk=wrong` must not be a coin toss about what reaches flash.
3. **The three JSON writers do not produce identical bytes.** picoserve escapes `/` as
   `\/`; `serde-json-core` spells `\u00XX` in upper case; `serde_json` does neither.
   All three parse all of it, so nothing in the product is affected - but the golden
   files are byte comparisons, so they contain neither character, and the harness
   enforces that. `tests/writers.rs` has the evidence.

## The rules this crate keeps

* **No PSK in a reply, ever** (spec section 8.4). No reply type has a field for one, and
  `tests/no_psk.rs` serialises a worst-case value of every one of them and greps the
  bytes. The single type that holds a PSK is `form::WifiForm`, a *request*, whose
  `Debug` prints the length and never the bytes and which has no `Display` and no
  `as_str`. The one way to the secret is `WifiForm::psk`, which a reviewer can grep for.
* **Sets of strings are enums.** A firmware that grows an idle mode and a Studio that
  has not heard of it fail loudly in one place instead of comparing strings in five.
  Where the set already exists elsewhere the conversion lives here and the other crate
  is untouched: `From<proto::IdleMode>`, `WifiState::from_u8`, `StreamState::from_u8`,
  `From<provision::FailReason>`, `From<proto::ErrorCode> for ErrorCode`.
* **`From<&Telemetry>`.** "A browser and a UDP sender see the same numbers" is code.
  `TelemetryReply` is the 48 bytes of spec 6.7 projected field for field, which is why
  its `state` and `last_codec` are raw bytes while `StatusReply`'s `state` is a word.
* **The SSID is bytes on the way in and text on the way out.** `text::ssid_text` returns
  `None` for a name that is not UTF-8, and the callers do the obvious thing: `ssid`
  becomes `null`, and a scan result that cannot be named is left out of the list rather
  than shown wrongly. Lossy conversion would change its length and mislead somebody
  comparing it with what they typed.
* **Every mutating request carries an optional `pin` and `counter`**, parsed and ignored
  (decision 3). `request::check_auth` is the one function parked card 041 will give
  teeth to; every mutating route already calls it.

## Why 16 networks

`NetworksReply` is capped at `MAX_NETWORKS`. The list is a `heapless::Vec<Network, 16>`
held in main RAM - the pool `docs/design/device-web.md` says breaks first - at 36 bytes
an entry, so the cap is 576 bytes of state and about 3.7 KB of worst-case JSON. It is
also more than anybody scrolls in a captive-portal sheet, and the list is strongest
first, so the sixteen that survive are the sixteen worth joining. A scan in a block of
flats really does find forty.

## Testing

`cargo test -p screeny-device-api`: 64 tests plus the crate-doc example, in well
under a second, with no hardware and no network.
`cargo check -p screeny-device-api --target thumbv7em-none-eabi` is the `no_std` gate.
`UPDATE_GOLDEN=1 cargo test -p screeny-device-api --test golden` rewrites the examples
after a deliberate change; read the diff before committing it.
