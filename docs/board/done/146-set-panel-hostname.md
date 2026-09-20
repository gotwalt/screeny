---
id: 146
title: set_panel takes an instance name or an address, and a hostname looks like neither
type: build
hardware: no
depends: [106]
owner: worker-146
branch: card/146-147-153-sender-fixes
---

## Goal

`POST /api/v1/set_panel {"to": "some-host:49374"}` either works or says why, instead
of reporting that there is no panel on the network.

## Context

Found while containerising the studio (card 107). `to` is parsed as a `SocketAddr`
and, failing that, treated as an mDNS instance name. A DNS hostname is neither, so
`{"on":true,"to":"host.docker.internal:49374"}` browsed `_screeny._udp.local.` for
three seconds, found nothing, and settled into:

```
"state":"connecting", "frames_offered":361, "frames_sent":0, "frames_dropped":361,
"last_error":"no screeny device found on _screeny._udp.local. after 3.0 s"
```

The operator's mistake was giving a name that needs a DNS lookup. The message says
the panel is missing. Those are different problems and the second one sends people
looking at the panel, the WiFi and the firmware.

This matters more in a container than it did on a laptop: `host.docker.internal` is
the documented way to reach the host from a bridge-networked container, and it is
the first thing anyone will type when trying the portable profile
(`docker-compose.portable.yml`). `docs/design/deployment.md` currently works around
it by telling people to use the literal address.

## Deliverables

- Decide and write down what `to` accepts. Suggested: a `SocketAddr`; a bare IP; an
  mDNS instance name; **or** a DNS name (with or without a port), resolved with
  `ToSocketAddrs` on the background task that already does the lookup. A `.local`
  name should try mDNS first and DNS second.
- When nothing resolves, `last_error` names what was tried: "`foo:49374` is not an
  address, and no `_screeny._udp` instance is called `foo`" rather than "no screeny
  device found".
- Tests in `crates/studio` (or wherever the parse lives) for each accepted shape and
  for the failure message.

## Acceptance

`{"to":"host.docker.internal:49374"}` from the portable container reaches a
`screeny-sim` on the host. A nonsense name fails within a second with a message that
names the name.

## Log

### Done, 2026-09-19 (worker-146)

Fixed in the library, not in the studio: `Target` in `crates/screeny` grew the
third shape and `crates/art`'s `target_for` is now one line delegating to it,
so `set_panel` gets it for free and the CLI, the art binary and the studio
cannot disagree about what a name means. (Card 170 owns `crates/studio`; I did
not touch it. See "What the studio should do" below - the answer is nothing,
it already works.)

**What `to` accepts**, decided and written into `Target::parse`'s doc comment,
`crates/screeny/README.md`, `crates/art/README.md` and
`docs/design/deployment.md`:

| you write | read as | resolved by |
|---|---|---|
| `10.0.0.5:49374`, `[::1]:49374`, `10.0.0.5`, `::1`, `[::1]` | an address | nothing; used as given |
| `host.docker.internal:49374`, `panel.lan`, `localhost:5000` | a host name - **a dot, or a port, or both** | the system resolver (`ToSocketAddrs`) |
| `screeny-4a00a4` | a DNS-SD instance name - no dots, no port | an mDNS browse, as before |
| `screeny-4a00a4.local` | a host name **first**, an instance name second | the resolver, then a browse for `screeny-4a00a4` |

Three decisions and why:

- **A bare undotted name stays an mDNS instance name.** It is what
  `screeny-4a00a4` is, it is what every existing caller passes, and changing
  it would silently turn a browse into a lookup. A port or a dot is how you
  say you mean a host. `--addr` is the exception and has its own constructor,
  `Target::direct`, where a bare name *is* a host: `--addr` exists to skip
  discovery, so it must never start a browse. That is also what `--addr`
  already did (`main.rs` resolved host names itself); the grammar moved into
  the library and the CLI kept its behaviour.
- **`.local` is the resolver first, the browse second.** The OS resolver
  answers `.local` on macOS and on a Linux host with nss-mdns, in
  milliseconds, and is the answer every other program on that machine gets;
  in a slim container it fails and the browse - which `mdns-sd` does for
  itself, needing no responder - picks it up. The other order would pay a
  three-second browse on every connect on the machines where the resolver
  already knows the answer. Measured cost of the order, on this Mac: a
  `.local` name that resolves is instant, one that does not costs **5.1 s** in
  `mDNSResponder` before the browse even starts (`screeny info --addr
  no-such-panel.local --timeout 0.4` took 5.56 s total). A dotted non-`.local`
  name that does not resolve costs **15 ms** and no browse at all.
- **A failed lookup is `Error::Unresolved`, not `Error::NotFound`.** That is
  the card: "the panel is not advertising" and "that name means nothing here"
  send people to different places. The new variant names the name and lists
  every lookup in order:

  ```
  "nothing-here.invalid:49374" is not an address, and did not resolve: the system
  resolver said: failed to lookup address information: nodename nor servname
  provided, or not known; it cannot be a _screeny._udp.local. instance name either
  (it has a dot in it), so nothing was browsed
  ```

  and for a `.local` name, `; browsing _screeny._udp.local. for an instance
  called "no-such-panel": no screeny device found ... after 0.4 s`. It carries
  a hint of its own pointing at `--addr`.

Also, deliberately:

- **Nothing blocks a render loop.** All of this happens inside
  `Target::resolve`, which `Link` already calls on its connect thread, and
  `Aim::Find` clones the target for *every* attempt - so a changed DNS answer
  is followed on reconnect exactly as a changed mDNS answer is. The embed test
  below restarts the device under the name to prove the second lookup happens.
- **IPv4 is preferred among the resolver's answers**, because the panel's
  stack and the simulator are IPv4; `localhost` must not come back as `::1`
  and miss a simulator bound to 127.0.0.1. An IPv6-only name still resolves.
- **A resolved host name yields an address and nothing else**, so the control
  port is frame + 1 (`Device::from_addr`) until `GET_INFO` corrects it, which
  is exactly what `--addr` has always done. `Device::instance` and
  `Device::host` are set to the name, so labels and logs say what was asked
  for.
- **Public API changes are additive**: new fields `Target::host` and
  `Target::port` (every construction in the tree uses `..Target::default()`;
  the one exhaustive literal was the CLI's own and is gone), new
  `Target::parse`, `Target::direct`, `Target::label`, new `Error::Unresolved`
  (the enum is `#[non_exhaustive]`). `art::target_for` and `label_of` keep
  their signatures.

Evidence:

- `cargo test --release -p screeny -p screeny-art`: all green, including five
  new unit tests in `discover.rs` (the grammar, labels, the `.local` ->
  instance rule, `localhost:port` actually resolving, and the failure
  message), two new in `tests/embed.rs` (a link that finds a sim **by host
  name** and reconnects to it after the device restarts; a name that resolves
  to nothing failing in under 2 s with `Unresolved`), and two in
  `crates/art/src/output/sender.rs` (the grammar through `target_for`, and a
  deferred `SenderOutput` reporting the bad name in `last_error` rather than
  "no screeny device found").
- End to end with the real binaries, the container's case in the form I am
  allowed to run: `screeny-sim --bind 127.0.0.1 --frame-port 49500 --no-mdns
  --headless`, then `screeny pattern bars --addr localhost:49500 --duration
  2`. The CLI printed `streaming bars to screeny sim at 127.0.0.1:49500` and
  the simulator logged `rx 62 shown 61 ... dec 0 rej 0`. The name was
  resolved, not browsed.
- Acceptance's second half: `screeny info --addr nothing-here.invalid` fails
  in **15 ms** naming the name (the card asked for one second).

**What the studio should do: nothing.** `set_panel` passes `to` to
`screeny_art::output::target_for`, so `{"to":"host.docker.internal:49374"}`
now resolves and `last_error` names the name when it cannot. Two things are
worth knowing there, neither a code change: a `.local` name that does not
resolve costs 5 s per connection attempt on macOS (on the connect thread, so
the engine is unaffected), and `resolve_aim` in `api.rs` still prefers a
registry device's instance name over its address, which is still the right
preference.

### Orchestrator (2026-09-20)

Merged to `main` cleanly. `cargo test --release -p screeny -p screeny-art`: green (203 passed across the
run). The only failures seen were two timing-sensitive `crates/studio/tests/fleet.rs` tests that fail
about one run in four on a loaded host and pass alone - not this branch; handed to card 170, which owns
those tests. Decisions accepted as made: resolver before browse for `.local`; `mean_bytes` keeps `FINAL`,
`mean_encode` drops it. Reaches the deployed Studio with the next deploy (card 170).
