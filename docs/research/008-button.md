# The Tidbyt Gen 1 button: which GPIO, and what to do with it (2026-09-19)

Card 202. `hardware: no` — nothing here was flashed and no serial port was opened.

## Conclusions

1. **The button is GPIO15.** Active low: pressed = 0. The stock firmware
   configures it as an input with the **internal pull-up** on and an
   **any-edge** interrupt, and its press test is literally
   `gpio_get_level(15) == 0`. Confidence: **very high** — two independent lines
   of evidence that never touched each other (a `gpio_config_t` struct decoded
   out of Tidbyt's flash image, and a stock boot log a user posted on Tidbyt's
   own forum).
2. **It is not wired to `EN`/`CHIP_PU`.** A pin on `EN` cannot be read by
   software at all, and the stock firmware reads this one continuously: it
   registers a GPIO ISR for it, polls it in a 5-second hold loop, and prints
   "Reset button released. Reset sequence aborted." when it goes high again.
   Whatever else the board does, the button reaches the ESP32 as a GPIO.
3. **It is not GPIO0**, so holding it at boot does *not* enter the ROM
   bootloader. Gen 1 owners who want download mode have to jumper IO0 to GND
   themselves. This is good news: the gesture design below can use a
   hold-at-boot without fighting the ROM.
4. **GPIO15 is dual-purpose on this board**, and `firmware/src/tidbyt.rs` is not
   wrong to call it `BOARD_ID_ADC_B`. The newer stock build ADC-reads GPIO13
   *and* GPIO15 (ADC unit 2, channels 4 and 3, 12-bit, 12 dB attenuation,
   4 samples averaged) to print `Hardware generation: gen%d, %dmV, %dmV`, and
   *then* uses GPIO15 as the button. See "The one loose end" below — it is the
   single thing the probe should settle on the bench.
5. **The stock gesture is: hold at boot, 5 seconds, erases NVS.** There is no
   short-press behaviour in the boot path; short presses go to a runtime
   handler with a 100 ms debounce.
6. `firmware/src/tidbyt.rs` should stop saying the pin is unknown. The constant
   becomes `pub const BUTTON_GPIO: Option<u8> = Some(15);` once the probe has
   confirmed it on the bench — a build card, below, does that.

## 1. The static analysis

The stock dump is `backup/tidbyt-stock-b48a0a4a00a4.bin` (8388608 bytes,
git-ignored). Everything below is reproducible from it; all extraction was done
in a scratch directory and deleted afterwards. Nothing from the image beyond the
few lines quoted here has been copied into this repo, and nothing should be: it
is Tidbyt's copyrighted binary.

### Which app is running

`otadata` at 0x e000 holds two 32-byte entries, both with valid CRC32 over their
sequence number and both in state `VALID`:

| slot | `ota_seq` |
|---|---|
| 0 | 11 |
| 1 | 10 |

The bootloader picks `(ota_seq - 1) % ota_app_count` = `(11 - 1) % 2` = 0, so
**app0 at 0x10000 is the running image**. `esptool image-info` on it:
project `tidbyt`, app version 33426, built `Feb 20 2024 18:39:47`, ESP-IDF
5.1.2, five segments. app1 at 0x400000 is version 35369, `Aug 6 2024` — a newer
*build* sitting in the *older* OTA slot. Both were analysed; they agree.

### A trap worth writing down

The flash MMU maps the DROM and IROM segments **eight bytes below** the load
addresses printed in the segment headers, because it maps 64 KB-aligned flash
pages to 64 KB-aligned virtual pages and the segment data starts at file offset
0x18. The working mapping for app0 is:

```
DROM va = 0x3f400000 + app_file_offset      (header says 0x3f400020 at 0x18)
IROM va = 0x400d0000 + app_file_offset - 0x60000
```

Using the header addresses finds *zero* references to *any* string in the whole
image, which looks exactly like "the strings are unreferenced" rather than "your
arithmetic is off by 8". The base was recovered empirically: take every
4-aligned word in the image whose value lands in DROM, and score candidate bases
by how many of them point at the first byte of a NUL-terminated string.
0x3f400018-for-file-offset-0x18 scores 2564 of 5410; the next best base scores
438.

### The decisive artefact

Xtensa `.literal` sections are all collected at the head of `.flash.text`, so a
module's literal pool is contiguous and easy to find. In app0:

```
DROM 0x3f402a7c  "tidbyt/button"
DROM 0x3f402a8c  "\e[0;32mI (%lu) %s: reset button event pressed\e[0m\n"
IROM 0x400d0538  literal -> 0x3f402a7c      (the TAG)
IROM 0x400d053c  literal -> 0x3f402a8c
IROM 0x400d0540  literal -> 0x3f402ac0      <-- a 24-byte const in DROM
IROM 0x400d0544  literal -> 0x400d5774      (the deferred press handler)
```

`0x3f402ac0` is a `gpio_config_t` template. Byte for byte:

```
00 80 00 00 00 00 00 00 | 01 00 00 00 | 01 00 00 00 | 00 00 00 00 | 03 00 00 00
pin_bit_mask = 0x0000000000008000   -> GPIO15, and only GPIO15
mode         = 1                    -> GPIO_MODE_INPUT
pull_up_en   = 1                    -> GPIO_PULLUP_ENABLE
pull_down_en = 0
intr_type    = 3                    -> GPIO_INTR_ANYEDGE
```

`tidbyt_button_init` (app0 `0x400d57a0`) copies those 24 bytes onto its stack and
hands them to `gpio_config`:

```
400d57a0:  entry   a1, 64
400d57a3:  l32r    a11, 0x400d0540      ; -> the const above
400d57a6:  movi    a12, 24              ; sizeof(gpio_config_t)
400d57a9:  or      a10, a1, a1          ; dst = stack
400d57ac:  call8   0x40091208           ; memcpy
400d57af:  movi.n  a10, 15              ; pin
400d57b1:  l32r    a11, 0x400d0544      ; -> handler 0x400d5774
400d57b4:  call8   0x400d6fd8           ; register per-pin callback + gpio_isr_handler_add
400d57b7:  mov.n   a10, a1
400d57b9:  call8   0x40121a44           ; gpio_config(&io_conf)
```

The press test, app0 `0x400d57c0`, is unambiguous:

```
400d57c0:  entry   a1, 32
400d57c3:  movi.n  a10, 15
400d57c5:  call8   0x4012182c           ; gpio_get_level
400d57c8:  movi.n  a8, 0
400d57ca:  movi.n  a2, 1
400d57cc:  movnez  a2, a8, a10          ; return (level == 0)
```

`0x4012182c` really is `gpio_get_level`: it loads the GPIO peripheral pointer,
compares the pin against 31, and reads word offset 0x3c (`GPIO_IN_REG`) or 0x40
(`GPIO_IN1_REG`) from the peripheral base, shifting the pin's bit down.

Two corroborations:

* app1, the Aug 2024 build, carries the identical 24 bytes at `0x3f402ce8` and
  the identical module structure. The pin did not move between builds.
* Scanning the whole of app0's DROM for plausible input-mode `gpio_config_t`
  structures yields exactly one isolated hit — GPIO15 — plus a cluster of
  coincidences inside one unrelated table.

### What the stock flow actually does

`tidbyt/boot`, app0 `0x400d3838`:

1. "Initializing high-resolution timers..." → `esp_timer_init` → "Initialized
   high-resolution timers."
2. `tidbyt_button_init`; on failure, log `Error in tidbyt_button_init: %s` and
   restart.
3. If the button is down *right now*:
   `W ... Reset button is being held. Keep holding for %d seconds to reset.` —
   the `%d` is a literal `movi.n a15, 5`.
4. Poll: re-read the button; if it comes up, `W ... Reset button released. Reset
   sequence aborted.` and carry on booting. Otherwise compare the uptime against
   the double constant `0x415312d0_00000000`, which is **5 000 000.0** (µs).
   Note the comparison is against the constant directly; I did not find a start
   timestamp being subtracted, so "5 seconds" is most likely *uptime reaches
   5 s*, and the button check happens around 0.9 s into boot. That matches the
   forum logs, where a hold from t=888 ms released at t=1528 ms aborted.
5. On timeout: `W ... Erasing NVS.` → `nvs_flash_erase`, then
   `Couldn't erase NVS: %s` on failure.
6. Normal boot continues: "Initializing non-volatile storage..." →
   `nvs_flash_init`, with the usual `ESP_ERR_NVS_NO_FREE_PAGES` (0x110D)
   erase-and-retry.

Runtime presses take a different path. `0x400d6fd8` stores the handler in a
per-pin callback table and calls `gpio_isr_handler_add`; the handler
(`0x400d5774`) **delays 100 ms, re-reads GPIO15, and returns if it is high** —
a 100 ms software debounce — before logging `reset button event pressed` and
calling the action. There is also a `wait_for(pressed)` helper at `0x400d57d4`
that polls GPIO15 every 10 ms until it reaches a requested state.

So the whole stock design in one line: **GPIO15, active low, internal pull-up,
any-edge interrupt, 100 ms debounce, and a 5-second hold at boot that erases
NVS.**

## 2. The independent confirmation

Found by web search, without reference to the disassembly:

* A user's stock Gen 1 boot log on Tidbyt's own forum contains
  `I (888) gpio: GPIO[15]| InputEn: 1| OutputEn: 0| OpenDrain: 0| Pullup: 1| Pulldown: 0| Intr:3`
  — the exact configuration decoded above, and the only input pin configured
  anywhere in the boot sequence. From the same user and device, on a boot where
  the button was held: `W (888) tidbyt/boot: Reset button is being held. Keep
  holding for 5 seconds to reset.` then `W (1528) ... released. Reset sequence
  aborted.` <https://discuss.tidbyt.com/t/couldnt-connect-to-the-internet/1094>
* Tidbyt's own support pages describe the Gen 1 control as "the blue reset
  button on the back of the device", held "about 5 seconds", and the setup flow
  as "hold the blue button, plug the cable back in, keep holding for 5 seconds".
  <https://help.tidbyt.com/changing-wifi-networks>,
  <https://discuss.tidbyt.com/t/setting-up-your-tidbyt/25/5>
  (Their factory-reset page describes a *black* button next to the USB port —
  that page is Gen 2. <https://help.tidbyt.com/factory-reset>)
* Gen 1 owners who want download mode have to **drill a second hole and add
  their own button**, or jumper IO0 to GND: the existing button is not on IO0.
  <https://discuss.tidbyt.com/t/flashing-firmware/3293>,
  <https://tronbyt.com/device-notes/>
* `tidbyt/hdk` has no button code at all, and its `sdkconfig` has
  `# CONFIG_BOOTLOADER_FACTORY_RESET is not set`.
  <https://github.com/tidbyt/hdk>
* `tronbyt/firmware-esp32` has a `CONFIG_BUTTON_PIN` (`main/Kconfig.projbuild`,
  default `-1`) and an active-low check in `main/main.c`, but sets it only for
  their own S3 boards (`CONFIG_BUTTON_PIN=1`). Every `sdkconfig.defaults.tidbyt-*`
  leaves it at `-1`, so tronbyt simply never reads the Gen 1 button.
  <https://github.com/tronbyt/firmware-esp32>
* Gen 2 replaced the mechanical button with a touch pad on `TOUCH_PAD_NUM8`
  (GPIO33) and moved the HUB75 clock onto GPIO15 — consistent with GPIO15 being
  the Gen 1 button and being freed up when the button went away.
  <https://github.com/tidbyt/hdk/blob/main/src/touch.c>

No teardown photo, iFixit page or usable FCC internal photo of a Gen 1 was
found, so the physical wiring (button to GND, presumably with an external
pull-up or relying on the internal one) is inferred, not seen.

## 3. The one loose end: GPIO15 is also a board-ID strap

app1 (the Aug 2024 build, not the one running) contains
`Couldn't adc read IO13` and `Couldn't adc read IO15`, and
`Hardware generation: gen%d, %dmV, %dmV`. Its detection routine
(`0x400d8d0c` in app1) creates an ADC oneshot unit with `unit_id = 1`
(`ADC_UNIT_2`), configures **channel 4** and **channel 3** at 12-bit with
attenuation 3, and averages four reads of each. On the ESP32, ADC2_CH4 is
GPIO13 and ADC2_CH3 is GPIO15 — which is exactly what the two error strings say.
app0, the running build, has neither string; it gets the display revision some
other way.

So GPIO15 carries both a board-identification voltage and the button. The two
are compatible only if the strap network is weak enough (or absent on this
revision) that the internal ~45 kΩ pull-up still takes the pin to a solid logic
high at rest. That is almost certainly the case — the stock firmware would not
work otherwise — but it is an assumption, and it is the thing worth measuring.
Practical consequences for us:

* We must **not** put a board-ID ADC read and the button on GPIO15 at the same
  time; we do neither today, and the colour-order question is already settled by
  measurement (`docs/research/004-first-bringup.md`).
* If we ever add board-revision detection, it reads GPIO15 **once, before**
  the button is configured, and it must tolerate the button being held (which
  would read ~0 mV).
* `firmware/src/tidbyt.rs`'s `BOARD_ID_ADC_B = 15` and a `BUTTON_GPIO = Some(15)`
  are both true and must sit next to each other with a comment saying so.

## 4. The probe firmware

`firmware/src/bin/gpio_probe.rs`, built only when its feature is on:

```
. ~/export-esp.sh
cd firmware && cargo build --release --features gpio-probe --bin gpio_probe
# ELF: firmware/target/xtensa-esp32-none-elf/release/gpio_probe
```

A plain `cargo build --release` does **not** build it — `[[bin]] gpio_probe` has
`required-features = ["gpio-probe"]` — and that was verified: building the
normal firmware after building the probe leaves the probe artifact untouched and
produces `screeny-fw` exactly as before. `espflash save-image` turns the ELF into
a 93,360-byte application image, so it is a valid, flashable binary; it has not
been flashed, because this card is `hardware: no`.

What it is: **no WiFi, no panel, no DMA, no esp-rtos, no embassy.** It configures
nine candidate pins as inputs and polls them every 1 ms. Nothing is driven, so
the panel stays dark and the brightness cap is not in play.

| probed | why | internal pull |
|---|---|---|
| 0 | boot strap, also on the CP2102N auto-reset | yes |
| 12 | MTDI flash-voltage strap | **no** — read only, per the bench rule |
| 13 | board-ID strap / ADC2_CH4 | yes |
| 14 | believed free | yes |
| 15 | **the expected answer** | yes |
| 34, 35, 36, 39 | input-only | none exist on these pins |

Not probed, deliberately: the 14 HUB75 lines, GPIO16/17 (PSRAM), GPIO1/3
(UART0 — this log), GPIO6-11 (flash).

It alternates two 25-second phases forever, announcing each:

* **phase A, pull-up** — a button to ground rests HIGH and reads LOW while held.
* **phase B, pull-down** — an unloaded pin rests LOW; a pin that stays HIGH is
  being held up by something external, which is how a board strap gives itself
  away. This is the phase that settles the loose end in section 3.

Each phase starts by printing the resting level of every pin. Then every level
change is logged as `[   1234 ms] GPIO15  HIGH -> LOW`, capped at **8 logged
edges per pin per second** — past the cap it prints
`GPIO15 capping at 8 edges/s — this pin is floating, not pressed` once and counts
the rest, so a floating pin cannot flood the port or hide the real answer. Every
5 s there is one summary line: every pin's current level, its edge count and its
dropped count.

**What the orchestrator should see, and what to ask the owner to do.** Flash it,
open the monitor at 115200, and ask the owner to press and release the button
three or four times during phase A, then hold it for about ten seconds, then do
the same again during phase B. Expected result:

* Phase A resting levels: `GPIO15 HIGH`. GPIO0 HIGH, GPIO13 and GPIO14 HIGH,
  GPIO12 whatever it is (no pull, so it may be noisy), the input-only pins
  whatever the board does with them, likely noisy and capped.
* Each press produces exactly two lines on GPIO15, `HIGH -> LOW` then
  `LOW -> HIGH`, with a gap that matches how long the owner held it, and
  **nothing on any other pin**.
* Phase B: if GPIO15 rests **LOW** with a pull-down, it has nothing external on
  it and the internal pull-up is what holds it up — the simplest case. If it
  rests **HIGH**, there is an external pull-up or a strap network, which is
  worth knowing before we rely on the internal pull-up alone.
* If some other pin moves instead, the static analysis is wrong and the pin is
  whatever moved. (I do not expect this.)

If the owner is not available: a paper clip briefly shorting nothing is not a
substitute, and there is no software-only way to finish this. The probe is the
whole answer; it takes one minute.

## 5. The gesture design

**One paragraph.** GPIO15 is an `Input` with `Pull::Up` and an any-edge
interrupt, owned by a single embassy task on **core 0** that spends its life in
`Input::wait_for_any_edge().await` and therefore costs the frame path exactly
nothing — no polling, no timer, no work on core 1, which stays devoted to the
panel. On a falling edge the task debounces by waiting 30 ms and re-reading (the
stock firmware uses 100 ms; 30 ms is enough for a tactile switch and keeps the
short press feeling instant), then races `wait_for_rising_edge()` against a
ladder of timers with `embassy_futures::select`. Release before 1 s is a **short
press**: show the identify/status screen — IP address, instance name, RSSI,
firmware version — for 10 s, then return to whatever was on screen, which is
also the answer to "which of these is which" when there is more than one device.
Crossing 1 s while still held starts a **countdown on the panel**, "WIPE WIFI
5… 4… 3…", one digit a second, drawn by the existing screens module; releasing
at any point during the countdown cancels it and shows "cancelled" for a second,
so the destructive action cannot happen by accident and the owner can always see
what is about to happen. Reaching **5 s held** wipes the stored WiFi credentials
and reboots into the captive portal (card 201's territory: this card only
specifies when the wipe is asked for, not how it is stored). Holding past **15 s**
escalates to a **factory reset** — every setting, not just WiFi — with the panel
switching to "FACTORY RESET 5… 4…" at 10 s so that the second stage is as
visible and as cancellable as the first; if the owner just leans on the button
forever, the action fires once at 15 s and then nothing more happens until they
let go. Boot is the same code path with one addition: after the button task
starts, if GPIO15 is already low, the firmware runs the countdown immediately, so
"hold the button while plugging it in" works exactly as it does on the stock
firmware and as Tidbyt's own support page tells people — and since the pin is
**not** GPIO0, holding it at boot has no effect on the ROM bootloader and cannot
strand the device in download mode. The one boot-time subtlety is that GPIO15 is
the MTDO strapping pin, which the ESP32 samples at reset to decide whether to
print the ROM boot log: a held button silences the ROM's output, which looks
alarming on serial but is harmless and is worth a comment in the code, because
someone will eventually spend an hour on it.

Sampling in detail, since it is the part that can go wrong:

* The task is on core 0's executor, beside `frames`/`control`/`mdns`. Core 1 is
  not touched.
* `Input::wait_for_any_edge()` is an interrupt-backed future; there is no poll
  loop and no periodic timer, so an untouched button costs zero CPU.
* Debounce is **30 ms settle, then re-read**. A bounce that has ended by then
  produces one event; a bounce that has not is re-armed and produces none. The
  same 30 ms applies to release.
* The panel countdown is drawn from the button task by asking the display for a
  screen, exactly the way the existing status screens are drawn. It must not
  hold the frame lock across a redraw — the lesson from card 007.
* A press that arrives while the countdown is being drawn is ignored; there is
  one button state machine and it is never re-entered.

Thresholds, in one table, so a build card can implement them without re-reading
the paragraph:

| held for | action |
|---|---|
| < 30 ms | ignored (bounce) |
| 30 ms – 1 s, then released | short press: identify/status screen for 10 s |
| 1 s – 5 s | on-panel "WIPE WIFI" countdown; release cancels |
| 5 s | wipe WiFi credentials, reboot into the portal |
| 5 s – 15 s | on-panel "FACTORY RESET" countdown from 10 s; release cancels |
| >= 15 s | factory reset: all settings, reboot |
| held at boot | the same ladder, starting at power-on |

## 6. The fallback, if the bench says the button is not GPIO15

Only relevant if the probe contradicts everything above — for instance if the
button turns out to be on `EN` after all on this particular unit. The fallback
is a **power-cycle gesture**: three power cycles, each less than 10 s after the
previous boot, wipes the WiFi credentials and comes up in the portal.

Behaviour: a counter lives in the settings store (card 200 owns the store; this
card specifies only what is stored and when). At boot the firmware reads the
counter, increments it, writes it back, and schedules a task for **10 s** after
boot that resets the counter to zero. So a boot that survives 10 seconds erases
the evidence, and only rapid, deliberate power cycles accumulate. On the boot
where the counter reaches 3, wipe WiFi, reset the counter, and enter the portal.
The panel should show the count ("2 of 3") for the first ten seconds so the
gesture is discoverable and so the owner can see it working.

Failure modes, all of which argue for keeping this as a fallback rather than a
feature:

* **Brown-out loop.** A marginal USB supply that resets the device repeatedly
  under panel load will reach three in well under a minute and wipe the WiFi
  credentials of a device nobody touched. Mitigation: only count a boot whose
  reset reason is a power-on reset (`esp_reset_reason` /
  `esp_hal`'s equivalent), never a brown-out, panic, watchdog or software reset.
  That removes most of the risk but not all of it, since a dying supply *does*
  produce power-on resets.
* **Flash wear.** A write on every boot. Trivial at human boot rates, not
  trivial if something is power-cycling the device in a loop; cap it by not
  writing when the counter is already 0 and the previous boot was clean.
* **Silent loss.** Unlike the button, there is no confirmation and no cancel.
  The panel countdown that makes the button gesture safe has no equivalent here:
  by the time the third boot happens, the decision is made.
* **A power strip is not a gesture.** Anyone switching off a whole desk at night
  and back on in the morning is safe (the gap is hours), but someone testing a
  new outlet is not.

Because of all of that: implement the fallback **only** if the button turns out
to be unusable, and keep the 10-second reset window short.

## 7. Proposed build cards

Titles and one paragraph each. No card files written — the orchestrator decides
what actually goes on the board and in which order. The free range for this
track is 230-239.

**Confirm the button pin on the bench (`hardware: yes`).** Flash
`firmware/target/xtensa-esp32-none-elf/release/gpio_probe` with the owner
present, run the two phases once, and record the serial transcript in this
document. Expected outcome: GPIO15 changes and nothing else does, and phase B
answers whether the pin has anything external on it. Then set
`tidbyt::BUTTON_GPIO = Some(15)`, replace the "do not guess it, measure it"
comment with the measurement, and note beside `BOARD_ID_ADC_B` that GPIO15 is
both the board-ID strap and the button. One flash, one minute of the owner's
time, and after it nothing in this area is a guess any more. Depends on nothing;
blocks everything else here.

**A button task with debounce and the short press.** Add a `button` module and
an embassy task on core 0 that owns GPIO15 as `Input` with `Pull::Up`, waits on
edges, debounces at 30 ms, and implements exactly one action: the short press
shows the identify/status screen for 10 s. No destructive behaviour at all in
this card. It is small, it is safe to leave running, and it proves the
interrupt path costs the frame pipeline nothing — the telemetry line should not
move when the button is mashed. Depends on the confirmation card.

**The hold ladder and the on-panel countdown.** Extend the button task with the
timer ladder from section 5 and the countdown screens, and make the two
destructive actions call into a settings-wipe API rather than implementing
storage themselves. Includes the boot-time case (button already low when the
task starts) and the comment about MTDO silencing the ROM boot log. The card is
finished when a hold can be cancelled at every point and the panel always says
what is about to happen. Depends on the button task and on card 200's store.

**Wire the wipe to the portal.** Connect "5 s held" to card 201's captive portal:
clear the stored credentials, reboot, come up in soft-AP mode with the settings
page. This is the card that makes the button do the thing the owner actually
asked for, and it is deliberately last because it needs the store and the portal
to exist first. Depends on 200 and 201.

**Fallback: the power-cycle gesture.** Only if the confirmation card says the
button is unusable. Implements section 6, gated on a power-on reset reason, with
the panel showing the count. Parked by default.

---

## Bench confirmation (card 203, 2026-09-20)

The orchestrator flashed `gpio_probe` with the owner at the panel pressing the button
(about 1 s down, 2 s up, with longer holds) for 80 s, across phase A (pull-up, 0-25 s),
phase B (pull-down, 25-50 s) and phase A again.

- **Edges were logged on GPIO15 and on no other candidate**: 11 HIGH -> LOW
  transitions, each followed by LOW -> HIGH, with hold times from 1.7 s to 8.4 s that
  match what the owner did. `BUTTON_GPIO` is now `Some(15)`.
- **The press works in phase B too** (e.g. `[30264 ms] HIGH -> LOW`, `[32170 ms] LOW ->
  HIGH`), and in an earlier run with nobody pressing GPIO15 rested HIGH in both phases.
  So the open question above is answered: something external holds the pin up (the
  board-ID strap network), the button pulls it firmly to ground against that, and the
  internal pull-up is belt and braces rather than the only thing keeping the pin high.
  GPIO0, 13 and 14 also rested HIGH under the internal pull-down in that run; 0 and 13
  have known external pull-ups, 14 was not expected to - not needed for anything here.
- **One bounce in eleven presses**: `[61643 ms] LOW -> HIGH`, `[61654 ms] HIGH -> LOW`,
  `[61795 ms] LOW -> HIGH` - an 11 ms glitch on release. The 30 ms debounce in the
  gesture design covers it.
- Owner's decision the same day: the gesture ladder as proposed, **including** the
  15 s factory reset.

Logs: `captures/card203-gpio-probe.log` (no presses) and
`captures/card203-gpio-probe-2.log` (git-ignored).
