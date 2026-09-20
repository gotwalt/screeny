---
id: 225
title: Spec - strike 8.1 (serial console), rewrite 8.3 to end at the portal, add the HTTP API section
type: docs
hardware: no
depends: [223, 226, 232]
owner: worker-225
branch: card/225-spec-portal-http
---

## Goal

`docs/design/protocol-v1.md` is normative and is behind the device. Bring sections 8.x in
line with what fw 0.5.1 does, and give the HTTP API a normative home. **Describe what
exists; design nothing new.** If code and spec disagree in a way that is not simply "the
spec is old", do not pick a side: list it in the Log for the orchestrator.

## Context (read first)

- `docs/design/device-web.md`: decisions 5, 6 and 10, "What the research settled", the
  card 223 paragraphs on `wifi_state` vs the sticky trial result.
- `docs/board/done/223-firmware-portal.md`, last section (the phone test): the captive
  answer is the setup page as a `200`, not a `302`; no DHCP option 114; both HTTP workers
  follow the soft-AP; the QR screen does not alternate; the `connected` screen yields to a
  stream.
- The shapes are already code: `crates/device-api` (routes, JSON bodies, error codes,
  golden files), `crates/provision` (states, `Timing::SPEC`), `crates/settings`.
  `crates/probe/src/http.rs` has the 38 rules the device is held to - the spec section
  should be the prose those rules are checking, and should cite rule numbers where useful.

## Steps

1. **8.1**: strike the serial console; say what superseded it (the portal, the settings
   page, `SET_WIFI`).
2. **8.3**: rewrite the join sequence from `crates/provision`'s machine: stored
   credentials -> (the `bench-wifi` build's compiled-in pair, test builds only) -> the
   portal; the trial join before commit; credentials stored only after they join; the
   online-origin trial that falls back to the stored network with a sticky `FAILED`; the
   30 s AP grace window; the 10-minute portal retry when credentials exist and nobody is
   on the AP; the 60 s link-down rule. Constants from `Timing::SPEC`, by name.
3. **New section, the HTTP API**: transport (port 80, no TLS, no auth - decision 3,
   `Connection: close`), the route table, request/response bodies and the error shape from
   `crates/device-api`, limits (body sizes, rate limits that exist), what is served on the
   setup network vs the LAN (`/` and `/setup`, the captive catch-all), and that a firmware
   upload route is reserved for cards 240/241 without specifying it yet.
4. **8.4**: check the security posture text still matches (open setup AP, PSK in clear
   during setup, unauthenticated LAN).
5. Cross-check every claim against the code, not against older docs. Keep the spec's
   existing voice, numbering style and line width.

## Exit

The three edits above in `docs/design/protocol-v1.md`; a Log list of every code/spec
disagreement found; `README.md` / `docs/README.md` pointers updated if section titles
changed. No code changes.

## Rules

Branch `card/225-spec-portal-http` in your own worktree; commit and Log as you go; do not
merge or push; no hardware, no LAN; bounded commands; never write a real SSID or password
(examples use `Example-Wifi1` / `password9`); touch nothing outside `docs/` and the card.

## Log
