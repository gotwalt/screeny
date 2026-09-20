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

### A post that arrives from the LAN, not from the portal

Spec 8.2 says `SET_WIFI` works from any state, and card 223's LAN settings page posts
credentials to a device that is already on the network. `Event::CredentialsPosted` is
therefore honoured in `Joining` and `Online` too, and `Trial::origin` remembers which
door it came in by (card 232):

```
   ┌───────────┐ CredentialsPosted  ┌───────────────────────┐ Joined  ┌──────────┐
   │  Online   │───────────────────▶│  Trial, origin=Online │────────▶│  Online  │
   │ or Joining│  StopJoin if one   │  NO AP, no portal, no │ commit  │ new net  │
   └───────────┘  is in flight,     │  PROVISIONING overlay,│ announce└──────────┘
         ▲        then StartJoin    │  panel left alone     │
         │        { Trial, 1 }      └───────────┬───────────┘
         │                                      │ failed (auth at once,
         │  StartJoin { Stored, 1 }              │ otherwise 3 attempts)
         └──────────────────────────────────────┘
            back to the PREVIOUS network, store untouched;
            wifi_state() reads FAILED until the next post or a reboot
```

* **The AP is never raised on this path.** There is one station: the device drops the
  association it has to try the new network, and there is nobody standing on a setup
  network to keep informed.
* **A failure goes back, not to the portal.** The store still holds the network that was
  working a moment ago. If *those* credentials then fail three times, the ordinary
  `Joining` -> `Portal` rule takes over. With an empty store the built-in credentials are
  tried, and with neither the portal is the only way back in.
* **The failure is sticky.** `wifi_state()` reads `FAILED` and `trial_is_current()` stays
  true even once the old network is back, until the next post or a reboot - otherwise the
  old network reconnecting a few seconds later reads as "your new network worked".
  Firmware 0.3.0's `SET_WIFI` behaves the same way.
* **No overlay, no panel.** `overlay_state()` gives `PROVISIONING` for a portal trial
  only, and `screen()` returns `None` throughout an `Online`-origin trial *and* after it
  succeeds: frames may still be arriving around the rejoin, the poster is reading the
  reply in a browser, and nothing about the panel is "in setup".
* `Boot` + `CredentialsPosted` is still ignored: nothing is serving before `Event::Boot`
  has been processed, so it cannot happen.
* A post that arrives while the soft-AP is still in its 30 s post-trial grace window is
  an `Online`-origin post all the same: the state decides, not the radio.

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
  reports to the portal page's full-page reload. `p.trial_is_current()` says whether
  `p.trial()` is still the answer that route should give, rather than the station's own
  state - it stays true for a failed LAN-side trial, which is the whole point of the
  sticky `FAILED` above.
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

## The one screen here that is not about provisioning

`Screen::Updating { percent }` (card 240) is drawn while a firmware image is
being written to the inactive slot. The state machine knows nothing about it and
should not learn: the firmware asks `crate::ota` whether an upload is in flight
and calls `render` with this variant directly. It lives here because the
*renderer* is here - the two things that can take the panel from a sender should
be drawn by one crate with host tests, not by two that look almost alike.

It is the only screen that outranks a live stream, which `docs/design/device-web.md`
decision 7 allows for a firmware update and for nothing else. The bar is an
outline that fills, so "no `Content-Length`, nothing to show yet" and "the panel
has died" do not look the same.

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
