---
id: 146
title: set_panel takes an instance name or an address, and a hostname looks like neither
type: build
hardware: no
depends: [106]
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
