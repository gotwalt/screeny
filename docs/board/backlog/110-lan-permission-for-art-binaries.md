---
id: 110
title: Local Network permission for the art binaries (screeny-art, screeny-studio)
type: build
hardware: yes
depends: [101, 015]
---

## Goal

Make `screeny-art play` and the studio's "send to panel" switch reach a panel on the
LAN from wherever they are actually launched, the way `screeny` already does, so the
first milestone - design a piece in the Studio, watch it on the panel - does not
depend on which window started the process.

## Context

macOS Local Network permission is **per binary identity**, and card 101 added two new
identities that talk to the LAN: `screeny-art` (the `sender` feature) and
`screeny-studio`. Until now the only one was `screeny`, and `CLAUDE.md`'s rule -
"build the sender with `tools/sign-macos.sh` when it will be launched outside a
terminal" - was written for it alone. A Tauri app is a bundle, which is exactly the
case the rule says is *not* exempt.

Card 101 hit this and could not work round it. Measured there, with no panel involved:
a `screeny-sim` bound to this Mac's own LAN address answered on loopback and did not
answer on the LAN address, and the **reference** `screeny` binary failed identically -

```
screeny info --addr 192.168.7.203:50608   ->  no reply ... after 4 tries (250 ms each)
screeny info --addr 127.0.0.1:50608       ->  answers instantly
```

- so it was the environment, not the code. mDNS browsing worked (the instance name
resolved to the right host and port); only the unicast that follows was dropped.
`screeny`'s error messages already name this cause, which is how it was diagnosed.

What is not yet known, and is most of the work:

- Whether the two new binaries need signing, a `com.apple.developer.networking...`
  entitlement, an Info.plist usage string, or only a first-run prompt that nobody has
  clicked yet.
- Whether a Tauri bundle needs anything the plain CLI does not.
- What a headless server build needs, since `docs/design/studio-vision.md` puts the
  studio in docker-compose on Linux where none of this exists. Do not let the macOS
  answer leak into the Linux path.

`tools/sign-macos.sh` and card 015 are the prior art.

## Deliverables

- `tools/sign-macos.sh` (or a sibling) covering `screeny-art` and `screeny-studio`.
- Whatever Info.plist / entitlement the studio bundle needs, in `crates/studio`.
- A paragraph in `crates/art/README.md` saying what to run before streaming from
  somewhere that is not a terminal, and what the failure looks like when you have not.

## Acceptance

- `screeny-art play <piece> --to screeny-4a00a4` streams to the panel when launched
  from Finder / a bundle / launchd, not only from an interactive shell.
- The studio's "send to panel" switch reaches the panel from the built app.
- The failure mode, when permission is missing, says so rather than timing out
  silently. (`screeny`'s message is the model; `SenderOutput` surfaces
  `PanelStatus::last_error` already.)

## Log

### Note from card 105 (2026-09-19): there is no bundle any more

Card 105 removed Tauri. `screeny-studio` is now a plain `target/<profile>/screeny-studio`
executable - the same kind of thing as `screeny` and `screeny-art` - so the two
questions in the Context about "a Tauri bundle" and "the built app" are moot: there is
one binary identity to sign, not a bundle to work out. The studio is also now built by
a plain `cargo build` at the workspace root (it is a default member again), so
whatever `tools/sign-macos.sh` grows should expect to sign three binaries from one
build rather than a binary and an app.

The Linux half of this card is unchanged and still matters: `docs/design/studio-vision.md`
puts the studio in a container where none of the macOS machinery exists, and the
server must not grow a macOS-shaped answer.
