---
id: 117
title: The soak failed once on a loaded bench, and said nothing useful about why
type: test
hardware: no
depends: []
---

## Goal

Find out whether `crates/studio/tests/soak.rs::the_server_survives_a_bounded_soak`
has a timing assumption that a loaded machine can break - and, either way, make
its failure say which property went.

## Context

Seen once, on 2026-09-20, by worker-196: a root `cargo test --release
--no-fail-fast` reported

```
test the_server_survives_a_bounded_soak ... FAILED
test result: FAILED. 0 passed; 1 failed; ... finished in 41.16s
```

while another worker's `cargo clippy --workspace` was saturating the machine.
**41 s** is short for a soak that normally takes 69 s, so it gave up early
rather than running out of time. It has not reproduced since:

- alone, `cargo test --release -p screeny-studio --test soak`: passed (69.0 s);
- deliberately under load, with `cargo build --release --workspace` running into
  a scratch target directory (load average 10-20): passed (68.8 s);
- the clean root run that followed: 720 passed, 0 failed.

Card 196's change cannot be the cause - the soak opens no WebSocket, and card
196 only touches what an open socket is sent - but that is an argument, not
evidence, which is the other reason to look.

The soak's assertions that a slow machine could plausibly break are the ones
about *rates and recency* rather than about facts:
`telemetry_ago < 10.0`, `ticks > base_ticks + 100`, `faults >= 3`, and the
`/healthz` 200 inside each round.

## Deliverables

- Whichever it is: a bound widened with a reason beside it, or a real fault
  found and fixed.
- A failure message on every one of those assertions that says the measured
  number, so the next sighting is one line rather than an investigation.
  (`assert!(x > y)` with no message is what made this card necessary.)

## Acceptance

The soak is run enough times under a deliberate load to say something honest
about its flake rate, and a failure now names the property and the number.

## Log
