# screeny-otastate

The OTA activate / confirm / revert state machine (card 241), as the one thing
in it that is pure reasoning: **what kind of boot is this**, and **has the image
on trial proved itself**. `no_std`, no alloc, no clock, no I/O, so the firmware
and `cargo test` run the same answers.

```
classify(booted, selected, state) -> Boot
    Settled            nothing to do; no otadata write happens on one of these
    Trial              confirm it or revert it
    Unproven           an activation was interrupted; mark it and try it anyway
    Reverted(reason)   the previous image is back; Deadline / Aborted / Rejected
    Unknown            the device cannot say what it is running: change nothing

decide(Health) -> Wait | Confirm | Revert
    confirm when WiFi has an address, the panel has swapped, and either
    somebody has made an HTTP request or 120 s have passed - but never before
    60 s; revert at 180 s. Research 006 section 6's numbers, in one place.
```

`booted` is the MMU's answer and `selected` is `otadata`'s, and **the
disagreement between them is how a rollback is detected**:
`esp-bootloader-esp-idf`'s `current_app_partition` works from `max(ota_seq)`
alone and does not look at the image states, so after the bootloader has rolled
an update back it still names the slot it rolled back *from*.

## `model`: the paper bootloader

Behind the `model` feature (on by default, off in the firmware) is `otadata`'s
two entries, `esp-bootloader-esp-idf 0.6.0`'s sequence arithmetic and ESP-IDF
`release/v6.1`'s selection code - the `PENDING_VERIFY -> ABORTED` loop that runs
on **any** reset, `NEW -> PENDING_VERIFY` on the selected entry, and
`ota_select_valid`. `tests/interruptions.rs` uses it to cut the power at every
instant of card 241's interruption table, twice over (an erased sector and a
torn one), and assert that the device always boots something and always boots
the right thing.

It is a model of two real implementations and is only as good as the reading
behind it; the citations are in the module's documentation, file and line. The
bench, not this crate, is what proves the real bootloader behaves this way -
card 241's own log has the procedure and what it found.
