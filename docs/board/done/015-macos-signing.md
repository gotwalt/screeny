---
id: 015
title: macOS code signing and Local Network permission for the sender
type: build
hardware: no
depends: [009]
owner:
branch:
---

## Goal

Make the sender binaries work reliably under macOS Local Network privacy, however
they are launched (Terminal, another app, launchd), without repeated prompts or
silent packet drops.

## Context

Raised by the owner 2026-09-19. macOS gates unicast LAN traffic and mDNS browsing per
code-signing identity. An unsigned or ad-hoc-signed Rust binary gets a new identity
every rebuild, so permission does not stick, and when the binary is launched by a GUI
app the decision is attributed to that app ("responsible process"). Denial is silent:
`sendto` fails with EHOSTUNREACH or mDNS browse returns nothing. Card 003's research
(section on macOS) has the known bugs, including cached denials after reboot.

So far (bring-up) plain `python3` and `dns-sd` launched from the Claude app could
send UDP and browse without trouble, so this is not blocking, but it will bite once we
ship a real binary.

## Deliverables

- Identity (decided 2026-09-19): sign with the owner's
  `Developer ID Application: Aaron Gotwalt (L2EG537FL9)` from the login keychain.
  Select it by name or team id, never by hash, and make it overridable with
  `SCREENY_SIGN_IDENTITY` so other machines can build. Never ad-hoc for anything
  long-lived. Notarisation is not needed for locally built binaries; note what it
  would take if the sender is ever distributed.
- `tools/sign-macos.sh`: signs `target/release/screeny` with a fixed identifier
  (e.g. `com.gotwalt.screeny`), hardened runtime, and an embedded Info.plist
  (`-sectcreate __TEXT __info_plist` at link time) carrying
  `NSLocalNetworkUsageDescription` and `NSBonjourServices = [_screeny._udp]`.
- Cargo post-build hook or `just`/`make` target so dev builds are signed consistently.
- Sender UX: detect the denial signatures (EHOSTUNREACH on unicast to a same-subnet
  address; empty browse) and print what to do (System Settings > Privacy & Security >
  Local Network), and always allow `--addr IP` to bypass discovery.
- Notes in `docs/` on how to reset a bad cached decision.

## Acceptance

A freshly rebuilt, signed sender run from Terminal, and one launched from a GUI
parent, both discover and stream to the panel after at most one permission prompt,
and keep working across rebuilds.

## Log

### 2026-09-19 - done by the orchestrator

- `crates/screeny/Info.plist` + `crates/screeny/build.rs`: the plist is linked into
  `__TEXT,__info_plist` of the `screeny` binary on macOS (`-sectcreate`). Carries
  `CFBundleIdentifier=com.gotwalt.screeny`, `NSLocalNetworkUsageDescription`,
  `NSBonjourServices=[_screeny._udp]`. Verified with `otool -s __TEXT __info_plist`.
- `tools/sign-macos.sh [--debug]`: builds, then
  `codesign --force --sign "Developer ID Application: Aaron Gotwalt (L2EG537FL9)"
  --identifier com.gotwalt.screeny --options runtime --timestamp=none`, verifies
  strictly and prints the result. `SCREENY_SIGN_IDENTITY` overrides (use `-` for ad hoc
  on machines without the certificate).
- Before: `Identifier=screeny-26abd4a0a82e2022`, `Signature=adhoc`,
  `Info.plist=not bound` - an identity that changes on every rebuild. After:
  `Identifier=com.gotwalt.screeny`, `TeamIdentifier=L2EG537FL9`, `flags=runtime`,
  `Info.plist entries=8`, "satisfies its Designated Requirement". The signed binary runs
  and `screeny discover` finds the bench device.
- Denial hints (EHOSTUNREACH / empty browse -> "System Settings > Privacy & Security >
  Local Network", `--addr` bypass) were implemented in card 009 (`Error::hint()`).
- Not done: notarisation (only needed to distribute; would need `--timestamp`, a zip
  and `notarytool`). Resetting a bad cached decision: `tccutil` cannot reset Local
  Network; toggle the entry in System Settings, or reboot if the known caching bug
  bites (see `docs/research/003-protocol-transport.md`, macOS section).
- Workflow: run `tools/sign-macos.sh` instead of a bare `cargo build` whenever the
  binary will be launched from anything other than a terminal. `cargo build` relinks
  and drops the signature back to ad hoc.
