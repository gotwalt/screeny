# screeny-settings - the flash layout contract

`no_std`, no alloc, no clock, no I/O. `sequential-storage 8.0.1`'s **map** over
the `screeny` partition, keyed by `u8`. This crate is the only place in the repo
that knows how a setting is laid out; the firmware supplies the flash and the
clock (card 212), and `cargo test` covers everything below on the host against
`sequential-storage`'s RAM `mock_flash`.

## The keys

| key | value | limit | absent means |
|---|---|---|---|
| 0 `SCHEMA_VERSION` | `u8` | - | blank partition: defaults, not an error |
| 1 `WIFI_SSID` | `&[u8]` | 32 | no credentials. **Zero bytes also means "no credentials"** - that is how `clear_wifi` works |
| 2 `WIFI_PSK` | `&[u8]` | 64 | open network (zero bytes is also an open network) |
| 3 `NAME` | `&[u8]`, UTF-8 | 32 | empty: use `screeny-<id>` |
| 4 `BRIGHTNESS` | `u8` | - | `DEFAULT_BRIGHTNESS` (96) |
| 5 `IDLE_MODE` | `u8` | - | `IdleMode::Status` |

Limits are `screeny-proto`'s constants, not numbers typed twice. An SSID is
**bytes** (real access points are not all UTF-8); a `Name` is **text**, because
it becomes a `heapless::String` in `screeny-receiver` and an mDNS instance name.

## Adding a key

1. Take the next number (6) in `store::key`. **Never reuse a retired one.**
2. Fetch it in `Store::load` and give it a `Fields` bit; a missing or unusable
   value must fall back to a default and be reported, never fail the load.
3. Add a saver that compares before it writes, so the debounce still works.
4. **Do not bump `SCHEMA_VERSION`.** An old build ignores keys it does not
   fetch, and a new build treats a missing key as "not set", so adding is
   already compatible both ways. Bump it only when an existing key changes
   meaning or encoding.
5. If the new value can exceed 64 bytes, raise `MAX_ITEM_LEN` and `SCRATCH_MIN`
   with it.

## The scratch buffer

`sequential-storage` holds no buffer, and neither does this crate: every call
takes the caller's. It must be

- **at least `SCRATCH_MIN` = 72 bytes.** The longest item is one key byte plus
  the 64-byte PSK, rounded up to the 4-byte flash word: 68. Every method checks
  this and refuses before touching flash.
- **4-byte aligned.** `sequential-storage` passes the caller's buffer straight
  to `NorFlash::write` (`item.rs`, `Item::write_new`), and `esp-storage`'s ESP32
  backend requires alignment. A plain `[0u8; 128]` on the stack is not
  guaranteed to be aligned. Use **`Scratch`** (`#[repr(align(4))]`,
  `SCRATCH_LEN` = 128), which is both.
- **owned by the caller, not by a future.** Research 006 section 2 measured an
  embassy task's future growing by 11 KB of `.bss` because buffers were held
  across an `await`. `Scratch` is 128 bytes wherever the caller puts it, and
  `Store` itself is one `MapStorage` with an uncached `Cache`: no key or page
  tables, so its own footprint is the flash handle plus a `Range<u32>`.

## The rules, and why

- **`load` cannot fail.** It returns `(Settings, LoadReport)`. Any error, any
  corruption, any value this build cannot interpret becomes "default for that
  setting, noted in the report". It never panics and it **never erases** - a
  factory reset is `erase_all`, and only a caller asks for it. The one place
  this crate erases on its own is a *save* that finds a foreign
  `SCHEMA_VERSION`, which is a migration: those items are unreadable by
  definition and are already being ignored.
- **Writes skip when flash already holds the value**, so a debounced slider
  that lands back where it started costs nothing.
- **Wi-Fi is written immediately, never debounced**: the device is about to
  drop its association. `debounce::Field` deliberately has no Wi-Fi variant.
- **`save_wifi`'s commit point is the SSID.** It clears the SSID, writes the
  PSK, then writes the SSID, so losing power part-way leaves the old
  credentials, none at all (the device comes up in the setup portal), or the new
  ones - never one network's name with another's key. Two keys cannot be written
  atomically; this is the next best thing.
- **`clear_wifi` writes a zero-length SSID** rather than removing the item:
  `remove_item` needs a `MultiwriteNorFlash` and the ESP32's internal flash is
  not one. The old PSK bytes stay physically in flash until that page recycles.
  Spec 8.4 says the PSK is not treated as a secret, so that is recorded rather
  than defended against; `erase_all` is the path that really removes it.
- **The PSK is never formatted** (spec 8.4). `Psk`'s `Debug` prints
  `Psk(<n bytes>)`, there is no `Display`, and `Psk::as_bytes` is the one way
  to the bytes - grep for it.

## Duplicated on purpose, for now

`DEFAULT_BRIGHTNESS` (96) is also `firmware/src/display.rs`'s. Card 212 should
make the firmware's constant an alias of this one; until then, changing either
means changing both.
