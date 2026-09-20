# screeny-provision

WiFi provisioning, minus the radio: the join/portal state machine as a pure function of
events and time, the `WIFI:` QR payload, and the portal screen. `no_std`, no allocation,
no float, no clock, no I/O. The firmware (card 223) and the simulator (card 224) both
drive it, so the behaviour cannot drift between them. Card 221; the design is
`docs/design/device-web.md` and `docs/research/007-device-web-and-portal.md` section 5.

## The machine

```
                 ┌──────────┐
   Event::Boot ─▶│   Boot   │ store has credentials? built-in ones?
                 └────┬─────┘
             yes ┌────┴────┐ neither
                 ▼         ▼
           ┌──────────┐  ┌──────────────────────────────┐
           │ Joining  │  │            Portal            │
           │ stored,  │  │ AP `screeny-<id>` up, QR on  │
           │ then     │  │ the panel, DHCP + DNS + HTTP │
           │ built-in │  │ on 192.168.4.1. No timeout.  │
           │ 3 x 15 s │  └───┬──────────────────────▲───┘
           └──┬────┬──┘      │ CredentialsPosted    │ JoinFailed
       Joined │    │ 3 fails ▼                      │ (AP never dropped,
              │    └──────▶┌───────────┐            │  store untouched)
              ▼            │   Trial   │────────────┘
        ┌───────────┐      │ AP stays  │
        │  Online   │◀─────│ up, NOTH- │
        │ AP down   │Joined│ ING saved │
        │ after 30 s│      └───────────┘
        └─────┬─────┘             ▲
              │ LinkDown > 60 s   │ Tick, 10 min, no AP client
              └───────────────────┘   (stored credentials only)

Event::ButtonWipe, from any state: ClearCredentials, then Portal.
```

## Actions

`step` returns up to four of these, in the order to carry them out.

| action | the caller does |
|---|---|
| `StartJoin { which, attempt }` | begin a join with the stored, built-in or just-posted credentials |
| `StopJoin` | abandon the join in flight |
| `RaiseAp` | APSTA soft-AP, DHCP, the DNS catch-all and the portal HTTP server up |
| `DropAp` | all of that down |
| `CommitCredentials { which }` | write to the settings store. The **only** commit, and never before a join succeeded |
| `ClearCredentials` | erase the stored credentials |
| `Announce` | mDNS and the LAN HTTP server up |

## Driving it from the firmware

```rust
let mut p = Provisioner::new(&Config { ap_ssid: "screeny-4a00a4", has_stored, .. });
for a in p.step(Event::Boot, now_ms()) { carry_out(a).await }
loop {
    let ev = select(radio_event(), ap_event(), http_post(), Timer::after_secs(1)).await;
    for a in p.step(ev, now_ms()) { carry_out(a).await }
    if let Some(s) = p.screen(now_ms()) { render(&s, frame)?; }
}
```

* `now_ms` is a free-running `u32` and is allowed to wrap (49.7 days). Every comparison
  inside is wrapping, so a wrap costs at most one late transition.
* A `Tick` once a second is plenty. Timeouts are measured from stored instants, not
  counted in ticks, so a late tick delays a transition and never loses one.
* **The machine never sees a password.** `Event::CredentialsPosted { ssid }` carries the
  SSID alone; hold the credential yourself and write it on `CommitCredentials`. Spec 8.4's
  PSK invariant is therefore structural: no type here has a field for one.
* Spec 8.2's "reply before disconnecting" is the caller's: answer the `POST` first, then
  call `step`.
* `p.overlay_state()` is the telemetry `state` byte overlay (`PROVISIONING`),
  `p.wifi_state()` is the `GET_WIFI` byte, and `p.trial()` is what `GET /api/v1/wifi`
  reports to the portal page's full-page reload.
* `p.screen(now)` returns `None` when the panel belongs to the normal idle/stream path,
  and never returns a screen `render` cannot draw.

## The QR, which is measured and not adjustable

Version 2, ECC L, **byte** mode, payload `WIFI:T:nopass;S:<ssid>;;` (exactly 32 bytes for
`screeny-4a00a4`), one LED per module, standard polarity - quiet zone and light modules
lit white, dark modules off - and a 3-pixel lit quiet zone, giving a 31x31 block in
columns 0..=30. The owner scanned that off the real panel on 2026-09-19. An SSID that
does not fit is **refused**, not silently grown to version 3, and gets the text-only
screen instead. `tests/render.rs` decodes the rendered frame with `rqrr`, which has never
seen our encoder, so none of this can rot quietly.

## Cost to the firmware

No `static`, no `.bss`. `Provisioner` 168 bytes, `Qr` 79, `Actions` 16, and `render`
puts two 80-byte QR scratch buffers on the caller's stack: roughly 400 bytes of stack at
its peak. The frame is the caller's existing buffer; this crate never owns one.

## Regenerating the pictures

```
cargo run -p screeny-provision --example portal-png
```

writes `docs/research/img/221-*.png` and prints the frame hashes. When a layout changes
on purpose: run it, look at the PNGs, paste the new hashes into `GOLDEN` in
`tests/render.rs`.
