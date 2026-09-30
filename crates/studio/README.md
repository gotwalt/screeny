# screeny-studio

Screeny Studio: the thing that plays generative art on the panels, and the browser
UI for designing it.

One ordinary Rust binary - an HTTP + WebSocket server with the UI compiled into it.
There is no desktop window and no Node toolchain; the same program runs on a laptop
and in a container ([`docs/design/studio-vision.md`](../../docs/design/studio-vision.md)).

```bash
cargo run --release -p screeny-studio       # http://127.0.0.1:8787/
cargo run --release -p screeny-studio -- --listen 0.0.0.0:8787 --state-dir /data
cargo run -p screeny-studio -- --ui-dir crates/studio/ui    # edit the UI, reload
```

Use `--release` for anything that streams: a debug build's encoder will not hold 30 fps.

**Several panels: channels** (cards 350-352, `docs/design/studio-vision.md`, "Several
panels"). A Studio drives every panel it finds. A **channel** is a running picture - a
patch, the named setting it came from, its working copy - rendered once per tick; a
**panel** is a device and everything about the device - its link, on/off, brightness,
output settings - and follows one channel, or none (**idle**: the device shows its own
screen). The same picture can be on several panels, in sync, because they are on one
channel; several pictures can play at once, because there are several channels. Every
screen is still a *window* onto what the panels are doing - for when they are not within
eyesight: the frames the browser draws for a panel are the same decoded datagrams that
panel is sent, and every control changes the panel it is about - a route or a socket
names one with `panel`, and without one it means **the first panel** (the first one
adopted). Card 351 gives the page its panel overview; until then it shows the first
panel, as it always showed the one panel.

There are **three screens** onto that one studio (cards 198, 301, 311), tied together by
one nav at the top of every one of them, because the work is three kinds of work:

| | |
|---|---|
| **Picture**, `/` | the canvas, what is playing, its parameters, and its named settings. Everything that changes or judges what the picture looks like, and nothing else. View - how *this browser* draws the panel - is a closed disclosure under the canvas: it never reaches the panel, so it is not a panel setting either. |
| **Panel**, `/panel` | which panel, whether the studio is even looking for one, the output switch, brightness, the panel model, the limiter, the link, what the device says about itself, identify / rename / reboot, "change which panel", and the studio's own health. |
| **Settings**, `/settings` | the studio's own settings, starting with Home Assistant (card 311): the broker, the device's name and id, and removing it. Card 303's Schedule screen had this place until card 310 retired modes and the timetable; `/schedule` now redirects (307) to `/`. |

Card 301 also simplified the Picture screen: Speed and the pause/restart controls
are gone - "I don't think speed should be varyable and start/stop is baffling in
this ui", the owner said - and so is the seed's quiet **Another** button ("zero
people understand it"). `set_seed`, `set_playback` and `restart` are still on the
API for a script; nothing in the browser calls them any more.

They are the same state and the same stream, so a change on one screen shows on the
others - and in another browser - at once. The Picture and Settings screens each carry
the same **status chip**, painted by one function (`showChip` in `common.js`, card 307):
the panel's name, what it is doing and (on the Picture screen, where there is a rate to
show) how fast it is being sent. It is the way to the Panel screen, and it takes the
fault tone and says why when the panel needs attention, so that trouble is never hidden
behind whichever tab a person happens to be on. The Panel screen has no need of a chip
pointing at itself. The Panel and Settings screens have no canvas and therefore ask the
socket for no frames at all.

`/dashboard`, which was a second app until card 170, is folded into `/` and redirects.

| flag | | env |
|---|---|---|
| `--listen ADDR` | where to serve. A bare number is a port on loopback. | `SCREENY_LISTEN` |
| `--state-dir DIR` | where what-plays-where is kept. Default `./.screeny-studio`. | `SCREENY_STATE_DIR` |
| `--ui-dir DIR` | serve the UI off disk instead of from the binary. | |
| `--no-discover` | do not browse or probe for panels; use configured addresses only. | |
| | offer two patches that misbehave on purpose. | `SCREENY_STUDIO_FAULTS=1` |

A flag beats the environment; the environment beats the default.

**`--listen 0.0.0.0:8787` has no password.** There is no authentication yet (a possible
later addition, 041), so anyone who can reach the port can change what is playing, and point the
studio at any panel on the network. That is the intended deployment on a home LAN or
over a tailnet, and it must not be published to the internet.

Patches, the pipeline, the panel model and the studio's meters are documented in
[`crates/art/README.md`](../art/README.md).

## What it is made of

```
 state file ──> device registry ──> panels (one per device, adopted idle)
      ^              ^                  │ each follows one channel, or none
      │  mDNS browse + typed addresses  v
      │                            channels ── one render per tick, handed to every panel on it
      │                                 │        │
      │                                 │        └─> per panel: its pipeline ─> screeny::Link ──UDP──> device
      │                                 │                                   └─> its frame cell ──WS ?panel=──> browsers
      └── every change                  └── supervisor (1 Hz): watchdog, fallback, adoption,
                                                               reconnect, brightness
```

**Channels render; panels show** (card 350, splitting card 106's player along the line
that was already inside it - the cross-fade blends in linear light *before* the limiter).
A channel renders one linear frame per tick and hands it to each panel on it; each panel
puts it through **its own** pipeline (limiter, panel model, quantise), fills **its own**
one-slot preview cell and sends it on **its own** link. So two panels on one channel move
in lock-step and can still differ in dither or limiter, and "what the browser is drawing
for a panel" and "what that panel is showing" are the same bytes by construction rather
than by agreement.

- **Devices are keyed by their own stable id** (the `id=` TXT key, or what `GET_INFO`
  answers), never by IP. A panel that takes a new DHCP lease is the same panel. A typed
  address is a *way of reaching* a panel and not a name for it: a device added by address
  gets a provisional `pending:<what was typed>` id and adopts its real one the first time
  it answers - the panel is renamed in place, keeping its channel and its place in line.
- **Every device is a panel, adopted idle** (card 350): whatever the browse finds or a
  human adds becomes a panel with no channel and no stream, named after its instance, so
  the device shows its own screen until somebody gives it a picture. Panels are kept in
  the order they were adopted; the first is what a route without `panel` means.
- **Picking a picture for a panel** - a patch, a named setting, Home Assistant - goes by
  three rules, in order: (1) another channel already shows exactly that picture (patch +
  named setting, no unsaved tweaks): the panel **joins** it; (2) the panel is alone on its
  channel: that channel **changes**, with its 2 s fade; (3) otherwise the panel **leaves**
  its group for a new channel. **Same as** (`/same_as`) joins another panel's channel,
  tweaks and all; **Detach** (`/detach`) gives a panel a copy of its channel with its own
  clock from then on. A slider edits the **channel**, so every panel on it changes
  together (the state says which others are on it: `shared_with`).
- **A panel that changes channel fades** over 2 s, in its own output stage: it keeps the
  old channel's frames while it fades (the old channel keeps rendering at full rate until
  it has) and blends them with `crossfade::blend`, before its limiter. A panel joining a
  channel follows that channel's clock; it does not restart it. A channel no panel follows
  is dropped once the last one has faded away from it.
- **A studio always has a picture**, even before it has a panel: with none found yet it is
  on the *unbound* stand-in panel, which has no device and no link. The first panel
  somebody **names** (`set_panel {"on":true,"to":...}`, `devices/add` with `play`) is
  renamed onto the stand-in, so the picture the browser is watching simply starts reaching
  the panel rather than restarting; a panel that is merely *found* is adopted idle and the
  stand-in goes.
- **Panel output off** (`POST /api/v1/set_panel {"on":false}`, or the switch on the
  page) releases the link with `FINAL` - every panel's, or one panel's with `panel`. The
  device goes back to its own idle screen and **stops receiving frames**; the page carries
  on showing the picture. That is the one way to look without touching a panel, and it is
  deliberately the only one.
- **Changes are drained, not thrown at a new thread.** A change goes into a one-slot
  mailbox that the render loop applies before its next frame, so dragging a slider -
  sixty changes a second - costs one re-read per frame. Only a panic or a stall
  replaces a render thread, which is why `health.restarts` counts faults.
- **Reaching a panel**: by instance name when a human typed a name, which re-resolves on
  every reconnect and so follows a DHCP lease; by `Link::attach` to its exact two ports
  when the registry has resolved it.
- **A panel that moves is followed** (card 141). A device known by *address* cannot
  re-resolve anything, so when it has been unheard for longer than `stale_after` the
  studio sends one `GET_INFO` to the subnet broadcast address (spec 5.5) on the browse's
  own tick. Every panel answers with its own `id=`, the registry is keyed by that id, and
  a known id at a new address is that panel: its address is updated and its channel, patch
  and seed carry on. One probe in flight, capped jittered backoff, one line in the log
  per panel followed and none per attempt, and off under `--no-discover`. A panel reached
  by name is left alone - it already follows itself - and an id this studio does not know
  is not adopted.

## Built to be forgotten

The point of the whole crate. It is expected to run for months with nobody opening the
page, so:

**Nothing grows without bound.** Preview frames live in a one-slot `watch` cell, state
changes in a fixed-depth broadcast, and a socket that cannot take a message in three
seconds is closed. There is one render thread per channel - one per picture, not one per
panel - one link per panel, one control request in flight, one browse and one probe at a
time, a one-slot mailbox in front of the state file, and every fault is logged *once*
rather than once a frame. A device that does not answer is polled on capped, jittered
backoff. A channel none of whose panels is connected or watched renders at 5 fps instead
of 30: a panel unplugged for a month must not cost a core for a month. A channel no panel
follows does not exist at all.

**A bad patch cannot take the process down.** A patch that panics is caught
(`catch_unwind`), logged once and replaced by a safe fallback in milliseconds. A patch
that *stalls* - a frame that never comes back - is caught by a five-second watchdog: the
wedged thread is told to stop and abandoned (no thread can be killed in Rust) and a fresh
core takes over. Links and pipelines belong to the panels and not to the core, so
abandoning one leaks a patch's render state and one thread, never a socket, a link thread
or a device's source lock. Three faults in a row and the channel stops trying and says so:
a restart loop is worse than a stopped picture. A fault is the channel's, so it is every
panel on it that falls back - and no other.

**The state file.** One small JSON file, `state.json`, in the state directory: what
devices are known, which panel follows which channel, what each channel shows, and every
named setting. Written atomically (temp file, `fsync`, rename) by one thread,
newest-wins, and not written at all when nothing has changed. It is versioned, and a
version this build does not understand is moved aside rather than parsed or deleted.
**Missing, empty, truncated, corrupt, wrong-typed or from the future all start a sane
default and say why once** - a state file is never a reason for the server not to run.

**What each panel is showing, tuning and all, and every named setting** are in that
same file and **nowhere else**: no second file, nothing in the working directory,
nothing in the browser's `localStorage`. That matters operationally - the container
mounts a volume at `SCREENY_STATE_DIR` and only what is written there survives an image
rebuild - and it is why a `docker restart` comes back showing exactly what it showed.

```jsonc
"version": 8,
"devices": [ { "id": "c0ffee", "name": "Desk", ... }, { "id": "d00d1e", ... } ],
"panels": [                                 // card 350, in the order they were adopted
  { "device": "c0ffee", "on": true, "channel": 3, "output": { ... }, "brightness": 96 },
  { "device": "d00d1e", "on": true, "channel": 3, "output": { ... }, "brightness": null },
  { "device": "beef01", "on": true, "output": { ... }, "brightness": null }  // no channel: idle
],
"channels": [                               // the pictures; one both panels above follow
  { "id": 3, "patch": "metaballs", "setting": "Lava", "seed": 111, "params": { "size": 2.5 } }
],
"patches": {                                // named settings, one library per patch
  "metaballs": {
    "settings": {                           // card 151
      "Lava":     { "seed": 111, "params": { "size": 2.5 }, "speed": 1.0 },
      "Slow ink": { "seed": 222, "params": {},              "speed": 1.0 }
    }
  }
},
"home_assistant": { "enabled": false, "host": "", "port": 1883, ... }   // card 311
```

Schema **v8** (card 350): `players` became `panels` and `channels`, `focus` went - which
panel a page looks at is the page's business now (`?panel=`) - and `patches` keeps the
named settings and **no longer keeps a working copy per patch**: a picture's tuning is its
channel's. A channel's `setting` is the name its working copy came from (absent is
Default); whether it has been moved since is computed, as it always was. The migration
gives every v7 player its own panel and channel, so the picture on each panel does not
change across the upgrade, and puts the focused one first, so a route without `panel`
still means the panel it meant. A patch whose working copy was showing nowhere and had
moved away from its setting is **named once**, in the `repaired` voice - that tuning has
nowhere to live in v8, and a named setting is what keeps one - and the v7 file is kept as
`state.v7.json`, as every migration's is. A hand-edited v8 file that names a panel twice,
points a panel at a channel that is not there, or puts a channel on a patch this build has
not got costs exactly that, said once (`repair_panels` in `src/state.rs`).

v7 (card 310) dropped `modes`, `schedule` and `schedule_run` - Home Assistant picks a
patch and setting now, and keeps the time - and a file that had any of them says so once,
naming what it lost (`note_retired_modes`), so the owner can set the same up on the HA
side. v7 also added `home_assistant` (card 311): the broker, the device's id and name,
kept nowhere else. v6 (card 302) had added modes and the schedule in the first place, and
retired speed and pause - `speed` keeps its place in a named setting for an older build's
sake, and every load puts it back to 1.0. Card 151 (v5) added a patch's named settings;
card 150 renamed three keys - `pieces` -> `patches`, a player's `piece` -> `patch` and
its `settings` -> `output` - and nothing else. Every old key is still read, and a file
older than v8 is copied to `state.vN.json` before it is migrated, so an older build can be
put back on the same volume.

v1 to v7 files are migrated in place, never thrown away, and a v1 file comes all the way
up in one start. v2's `preview` block - the design view's own patch, back when it had
one - was merged into the per-patch memory where it knew nothing about that patch, and a
v2 file with **no** players, where the design view was the only thing playing, becomes a
panel (the unbound stand-in, or the device `panel_to` named) rather than losing what it
was showing.

**Only what differs from the patch's defaults is stored**, on a channel and in a setting,
so a later release's better default still reaches everybody who never moved that slider,
and the file stays small. The seed is kept with the parameters; the pipeline `output`
(panel model, dither, limiter) is not part of a picture - it is a panel's.

**A stored value can never break a patch.** Patches gain, lose and re-range parameters
between releases, so every value is checked against this build's own spec on the way in,
one value at a time: a parameter that has gone away is ignored, one outside the range is
clamped to it (which is what the slider would do), and one that is not a finite number at
all goes back to the patch's default. One bad value never costs the rest of a setting,
and - this is the part worth stating, because it is the difference between a repair and
card 106's `state.bad.json` - it never costs the rest of the file. The settings of a patch
this build has not got are **kept**, so a patch that comes back in a later release comes
back with them; at most 64 such entries are kept, so a hand-edited file cannot grow for
ever. Whatever had to be corrected is said once, on the way in, and appears under
`state.repaired` in `/api/v1/status`. It is not a fault and never a 503.

"Reset" (`POST /reset_params`, or `player/set {reset_params: true}`) means *back to the
defaults*: the channel's parameters go back to the patch's own. It leaves the seed alone,
which is what Reset is about. On the
page it is not a button any more: it is loading **Default**, which does the same and more
(see below).

## A patch has named settings (card 151)

The model, in four words the rest of this section uses:

| | |
|---|---|
| **patch** | metaballs. What plays. |
| **working copy** | what a picture is set to *now*: its parameters and its seed. Since card 350 there is one per **channel** - it is what the page's sliders move, so every panel on the channel moves with it - and it survives a restart. (Cards 151-310 kept one per patch; a patch picked afresh now arrives on its Default.) |
| **setting** | a working copy saved under a name. A patch can have up to 64. They belong to the **patch**, one library for the whole studio, not to a panel or a channel: every panel has the same settings to pick from. |
| **Default** | the setting every patch has and nobody can change: its declared parameter defaults, speed 1.00x and a fixed seed. Not stored - synthesised - so a release that improves a default improves Default for everybody. |

**"Modified" is computed, never stored.** It is the working copy compared with the
setting it says it came from (or with Default when it came from nowhere), so it cannot be
left behind by a change that forgot to clear a flag. The comparison is made against the
setting *as this build can use it*, which matters the day a patch gains or loses a
parameter: otherwise a setting that had to be repaired would read as modified for ever.

**A setting outlives the patch it was saved from.** A parameter the patch has since lost
is dropped on load, one it has gained is simply absent and therefore at its default, and
one that is now out of range is clamped - each said once in the same `repaired` voice as
the rest of the state file, and never an error. What is on disk is left as it was, so a
build that has the parameter back gets the value back.

The **seed** is in a setting, and in the state file, and on the API, and nowhere on the
page: the number is opaque and says nothing about what is on the panel (the author, 2026-09-20).
What a person wants from it is "show me another one like this", which used to be the one
quiet **Another** button beside the settings control - shown only for a patch whose
picture really depends on its seed (`PatchDef::seeded`, set per patch by reading its
code). Card 301 took it off the page entirely ("zero people understand it", the owner
said), so `set_seed` is reached from the API now and not from the browser. A patch that
composes as it goes has better words of its own - the clocks' "Compose another", the
dials' "Move on" - and those stay, unaffected, under *Now playing*. The number itself
is still in `/api/v1/status`, for reproducing a frame.

Names are trimmed, 1 to 40 characters, unique within a patch however they are spelled
(`Lava` and `lava` are one setting), and `Default` is reserved in any case. A refusal is a
**400 with a sentence**, which the page puts on the line beside the control.

Each takes `panel` (card 350): the setting is about that panel's channel.

| route | body | answer |
|---|---|---|
| `POST /settings/load` | `{name, panel?}` - `Default`, or empty for the one it is on ("Revert"). **A picture pick**: card 350's three rules, so a panel sharing its channel that loads something else splits off, and one that loads what another channel is already showing joins it | the panel's new state |
| `POST /settings/save` | `{name?, panel?}` - a name is "Save as...", no name overwrites the one its channel is on | the panel's new state |
| `POST /settings/rename` | `{from?, to, panel?}` - no `from` is the one its channel is on; every channel on it follows the name | the panel's new state |
| `POST /settings/delete` | `{name?, panel?}` - every channel that was on it is then on Default, and reads as modified | the panel's new state |
| `POST /set_picture` | `{patch, setting?, panel?}` | a patch on one of its settings, in one step (card 350) |
| `POST /player/set` | `{device, setting}` | the same load, by device |

```sh
curl -s -X POST -H 'content-type: application/json' \
     -d '{"name":"Lava"}' localhost:8787/api/v1/settings/save
#  -> {"patch":"metaballs","setting":"Lava","settings":["Lava"],"modified":false,...}
```

The **list**, the **current name** and **`modified`** travel in `StudioState`, so every
answer and every broadcast carries them and no browser needs a second read: a save, a
load, a rename or a delete in one browser shows in another at once. **Loading is one
change** - the parameters and the seed move together, one broadcast and one write - so
the panel follows in one step rather than through a burst of half-loaded pictures.

Deleting a setting **does not change what is playing**: the values stay, the name goes,
so every channel that was on it is on Default and honestly marked modified.

> The panel's own firmware serves a `POST /api/v1/settings` of its own
> ([`docs/design/device-web.md`](../../docs/design/device-web.md)) for its WiFi and its
> name. That one is on the device, on port 80, and has nothing to do with these: nothing
> is shared but the word.

Putting a good setting into the repo as a factory setting was offered to the author and not
chosen, so it is not built. A setting is plain data in `state.json`, so nothing stops it
later.

A **v1** state file - what a service deployed before card 151 has - is migrated: what
each panel was playing comes up on its channel, tuning and all (the design view's own
patch, showing nowhere, is named once; see the state file above).

**Stopping is clean.** Ctrl-C and `SIGTERM` (what `docker stop` sends) release every
panel with `FINAL` and flush the state file, rather than leaving the panels on the last
frame until their stream timeout.

Card 302's modes and timetable - a schedule built and run inside the studio - were
retired by card 310: Home Assistant picks a patch and setting and keeps the time now.

## Home Assistant (cards 308, 310, 311)

**One HA device per panel (card 352).** Every panel the studio has is a device in Home
Assistant, each with the five entities below. The **first panel** - the one a route
without `panel` means, the first adopted - is the device every earlier build announced,
with **the same discovery id, topics and `unique_id`s, byte for byte** (a test pins them
against the strings the single-device build published: `ha::fleet::tests`), so current
automations and dashboards keep working, and it is still named by the Device name on the
Settings screen. Every **later** panel is a device of its own, named after the panel
(what the Devices list calls it), keyed by its device id. Picking a picture in HA goes by
the three picking rules like any other pick (`Panels::pick`, the page's own path): two
panels set to the same picture share a channel and stay in sync.

| | first panel | later panel (key = its device id, made safe for a topic: `[a-zA-Z0-9_-]`) |
|---|---|---|
| discovery | `homeassistant/device/screeny_<id>/config` | `homeassistant/device/screeny_<id>_<key>/config` |
| device identifier, `unique_id` stem | `screeny_<id>` / `screeny_<id>_<entity>` | `screeny_<id>_<key>` / `screeny_<id>_<key>_<entity>` |
| entity topics | `screeny/<id>/<entity>/state\|set` | `screeny/<id>/<key>/<entity>/state\|set` |
| availability | `screeny/<id>/status` | the same one: an MQTT connection has one Last Will |
| device name | Device name (Settings) | the panel's name; a rename updates the device |

A panel adopted while the studio is connected gets its discovery published within a
second, a **forgotten panel's device is removed** (an empty retained config, and every
one of its retained states cleared - HA drops the device), and a rename is a new config
with the same identifier, so HA renames the device and keeps its entities. The studio is
watched, not notified: the snapshot is taken on every state event and once a second, so
none of that needs a hook. "Remove from Home Assistant" clears every panel's device.
Limits: a panel's key follows its device id, so a manually typed address that later
resolves to its real id (`addr_...` -> `screeny-4a00a4`) is a new HA device; and if the
first panel is forgotten the next one becomes the first and takes over the plain ids.

The studio shows up in Home Assistant through **MQTT discovery**: HA's own MQTT
integration, no custom component, no YAML on the HA side. HA can see what is playing,
set the brightness, and pick a picture. Brightness is **its own control**, separate
from what is on the panel (owner, 2026-09-26), so an HA automation can follow the
room's light sensor while a schedule - now an HA automation of its own - decides the
picture. *"Time of day is Home Assistant's business entirely"* (card 310): the studio
has no modes and no timetable any more, and a schedule is just an automation that sets
the Picture entity below.

**Set up on the Settings screen** (`/settings`, card 311), not the environment: Connect
switch, Broker, Port, Username, Password (write-only - the API only ever says whether
one is set), and under "More" the Device name, the Id, the discovery prefix
and the resulting discovery topic and topic base. Saving restarts the connection with
the new settings at once, and the screen reports how it is doing. The settings live in
`state.json` under `home_assistant` (schema v7) - the password in the clear, on the
studio's own volume, and **nowhere else**: the API never sends it back out. The
environment variables an earlier build read (`SCREENY_MQTT_*`), the `--mqtt-forget`
flag and the compose secret are gone with them; `Config::mqtt` remains only as the
settings a test starts already connected with.

**Entities**, per panel - shown here as the first panel's, one device (`screeny_<id>`),
one retained config at `homeassistant/device/screeny_<id>/config`, with every entity in
its `components` map (a later panel's are the same under the layout above):

| entity (default id) | platform | topics under `screeny/<id>/` | payload |
|---|---|---|---|
| `light.screeny` | light, JSON schema, brightness only | `brightness/state`, `brightness/set` | `{"state":"ON","brightness":96}`, 0-255, snapped to the panel's nearest real step (card 187). `OFF` is a dark panel (the studio carries on playing); a bare `ON` goes back to the last lit level (128 if there was none). |
| `number.screeny_brightness` | number, "Brightness" | `level/state`, `level/set` | `0`-`100` in steps of 4 (one output-enable slot of the panel's 25, card 187 - every step is a real change). HA shows this inline on the device's own page, which it does not do for a light's brightness. |
| `select.screeny_picture` | select, "Picture" | `picture/state`, `picture/set` | every playable patch, on Default and on each of its named settings: `Vesta`, `Vesta · Wall Clock`, ... The list follows the named settings (discovery is republished when one is saved, renamed or deleted). The state is the current patch and setting's label, or `None` (unknown) when the working copy has been modified since the setting was loaded. Picking one is a hand change with the usual 2 s fade. |
| `sensor.screeny_patch` | sensor | `patch/state` | `{"id":"overland","name":"Overland","setting":"Dusk","modified":false}`; the state is `name`, the rest are attributes |
| `binary_sensor.screeny_panel_link` | binary sensor, `connectivity`, diagnostic | `panel/state` | `ON` while the studio is driving the panel and the link is up |

Availability for all of them, on every panel, is `screeny/<id>/status`: `online` / `offline`, retained,
with `offline` as the Last Will. Every state is retained, so HA has the right values after
its own restart. A command that is not valid for its entity (a picture that is not in the
list, brightness above 255) is refused and logged, and changes nothing.

**Retired, and removed from HA.** Card 310 took modes and the timetable out of the
studio, and with them the `scene` select, the `schedule` switch, the `resume` button and
the `scheduled` sensor. They are listed in `ha::discovery::RETIRED` and announced on
every connect as a bare `{"platform": ...}` - which is how device discovery removes a
component while keeping the rest - so a studio that was down when this build first ran
still cleans up after itself; their retained state topics are cleared on every connect
too (`Topics::retired_state_topics`).

**Lifecycle.** On every connect, first or after a broker restart, the studio
resubscribes to the command topics (`screeny/<id>/+/set` and `screeny/<id>/+/+/set`, so a
panel adopted later needs no new subscription; a command for a panel that is gone is
dropped) and to `homeassistant/status`, then publishes the
config, `online`, and every state. When HA says `online` there (HA restarted), it
publishes the config and every state again. A lost broker is retried with a backoff up
to 30 s, for as long as the studio runs, and logged once rather than once per retry.
Stopping publishes `offline` before disconnecting.

**Removing it from HA:** the Settings screen's "Remove from Home Assistant" button
(`POST /api/v1/home_assistant/forget`) switches the integration off, then connects once
more to clear everything it left retained - the config and every retained state topic -
so HA drops the device and its entities. The settings are kept, switched off, so
switching it back on brings the device back.

| route | body | answer |
|---|---|---|
| `GET /home_assistant` | | `HaView`: `enabled`, `host`, `port`, `username`, `password_set`, `discovery_prefix`, `instance`, `name`, `status` (`{state: "off"\|"connecting"\|"connected"\|"failed", detail?}`), `discovery_topic`, `topic_base` |
| `POST /home_assistant/set` | every field optional; absent is unchanged, an absent `password` keeps the saved one and `""` clears it | the new `HaView`, or a 400 with a sentence (e.g. switched on with no broker) |
| `POST /home_assistant/forget` | `{}` | the new `HaView`, switched off |

**Trying it against a local broker**, now through the API rather than the environment:

```sh
docker run -d --rm --name screeny-mqtt -p 127.0.0.1:18830:1883 \
    eclipse-mosquitto:2 mosquitto -c /mosquitto-no-auth.conf
mosquitto_sub -h 127.0.0.1 -p 18830 -v -t '#' &        # watch everything
cargo run -p screeny-studio -- --no-discover --state-dir /tmp/studio-mqtt
# from another terminal, once the studio is up:
curl -s -X POST -H 'content-type: application/json' \
     -d '{"enabled":true,"host":"127.0.0.1","port":18830}' \
     http://127.0.0.1:8787/api/v1/home_assistant/set
#  -> homeassistant/device/screeny_studio/config {"device":{...},"origin":{...},"components":{...}}
#     screeny/studio/status online
#     screeny/studio/patch/state {"id":"clocks-numerals","name":"Clocks: numerals",...}
#     screeny/studio/brightness/state {"state":null}      (until something sets one)
#     screeny/studio/picture/state None
#     ...
#  with a second panel (device id screeny-4a00a5) adopted, as well:
#     homeassistant/device/screeny_studio_screeny-4a00a5/config {...}
#     screeny/studio/screeny-4a00a5/picture/state None
#     ...
mosquitto_pub -h 127.0.0.1 -p 18830 -t screeny/studio/level/set -m '40'
#  -> screeny/studio/brightness/state {"state":"ON","brightness":97,"color_mode":"brightness"}
#     screeny/studio/level/state 40
mosquitto_pub -h 127.0.0.1 -p 18830 -t screeny/studio/picture/set -m 'Vesta'
#  -> screeny/studio/picture/state Vesta               (the panel cross-fades to it over 2 s)
mosquitto_pub -h 127.0.0.1 -p 18830 -t screeny/studio/screeny-4a00a5/picture/set -m 'Vesta'
#  -> screeny/studio/screeny-4a00a5/picture/state Vesta  (the second panel; it joins the
#     first panel's channel, one picture in sync on both)
# Ctrl-C the studio:
#  -> screeny/studio/status offline
docker stop screeny-mqtt
```

Two HA-side examples - an automation is Home Assistant's own YAML, not anything this
studio reads:

```yaml
# A schedule: pick the picture by time of day.
automation:
  - alias: Screeny night picture
    trigger:
      - platform: time
        at: "22:00:00"
    action:
      - service: select.select_option
        target: { entity_id: select.screeny_picture }
        data: { option: "Vesta" }
  - alias: Screeny day picture
    trigger:
      - platform: time
        at: "07:00:00"
    action:
      - service: select.select_option
        target: { entity_id: select.screeny_picture }
        data: { option: "Flock" }

# Brightness following a lux sensor.
automation:
  - alias: Screeny brightness follows the room
    trigger:
      - platform: state
        entity_id: sensor.living_room_lux
    action:
      - service: number.set_value
        target: { entity_id: number.screeny_brightness }
        data: { value: "{{ (states('sensor.living_room_lux') | float / 10) | round(0) }}" }
```

The same broker runs the end-to-end test, which is ignored by default because it needs one:
`SCREENY_TEST_MQTT=127.0.0.1:18830 cargo test -p screeny-studio --test ha_mqtt -- --ignored`.
It uses the discovery prefix `screeny_test`, so it cannot put entities in front of a real
HA even when pointed at the house broker. It covers the picture select, the brightness
slider, the settings routes, reconnecting, and forgetting - and, against two
`screeny-sim` panels, `every_panel_is_an_ha_device` (card 352): two discovery configs,
the first panel's ids unchanged, a pick on panel 2 leaving panel 1 alone, the same picture
on both being one channel, a rename and a forget. The module is `src/ha/`:
`discovery.rs` (the config, as types), `payload.rs` (states out, commands in), `client.rs`
(the connection and its lifecycle), `fleet.rs` (card 352: what several panels look like on the broker, pure), and
`bridge.rs`, the one file that knows the studio.
The payload snapshots are in `src/ha/snapshots/`; `SCREENY_BLESS=1` rewrites them for a
change that is meant.

## Health: what 503 means

`GET /healthz` is **200 `ok`**, or **503** and the reasons in words.

**It is about the server, not about the panels.** A panel that is unplugged, switched
off, rebooting or on the wrong side of a dead access point is normal life for something
that runs for months, and restarting the container is never the right answer to it. A
missing panel, a link that is `connecting`, a device that has not been heard from and an
empty device list are all **200**.

503 is the three ways the *process* can be broken, each of which a restart genuinely
does fix:

1. **the state file cannot be written** - a studio that cannot save will not come back
   as itself;
2. **a channel has given up** - three panics or stalls in a row, so it is no longer
   trying;
3. **a channel is not running**, and has not been for longer than the fifteen-second
   start grace - a render thread that died and was not replaced. An idle panel has no
   channel and is not this: it has nothing to render.

A **missing graphics adapter is not one of them** (card 145). A studio with no GPU plays
every CPU patch perfectly well and no restart conjures one, so it is reported as `gpu` on
`/api/v1/status` and said on the page, never as a 503.

Card 106 had a fourth, "the preview engine is wedged", and card 170 deleted the thing
it was about. A patch that stops returning is now caught by the same five-second
watchdog every channel has, abandoned, and replaced by the fallback - so it is recovered
in seconds rather than waiting for somebody to restart the container. (That was card
143, closed by construction.)

`GET /api/v1/status` is the same judgement with everything behind it: per device, the
last frame sent, the last telemetry heard, fps, drops by cause, RSSI, uptime, reconnects
and what it is playing - and, since card 180, `facts`: what only the panel knows, read
from **the panel's own** `GET /api/v1/status` over HTTP. **Both answer even while a patch is wedged**, because that
is the moment somebody wants them. Card 106 had to work at this - the design view's
engine lived behind a `Mutex` that a stuck patch held for ever, so the heartbeat cached
the last readable view. A channel's core is owned by its own render thread and is behind
no shared lock at all, so there is nothing left for a stuck patch to hold.

### Reading the panel's own status (card 180)

Firmware 0.4.0 and later serve an HTTP API of their own on port 80
(`docs/design/device-web.md`), and `GET /api/v1/status` there carries what UDP
telemetry cannot: heap, free stack, firmware slot and `otadata` state, why the chip last
restarted, errors in the device's own settings store, the WiFi state and SSID,
and `boot_id`. The studio
reads it, merges it with the telemetry rather than replacing it, and puts it on the page
under **Device**. `boot_id` changing is the panel rebooting, and that is the only thing
reboots are counted from - never uptime.

**The panel is fragile in exactly one way, and the whole design of this is that one
rule.** It has one connection worker and no listen backlog, so a second simultaneous
connection is dropped at SYN and costs a second of retransmit. So: one poller task
(`fleet::spawn_device_http`), which `await`s each read before starting the next - one
connection in flight across the whole fleet, not one per panel; no faster than
`MIN_DEVICE_HTTP_EVERY` (10 s), which is what `Config::default` carries and what `main`
sets; `Connection: close`, a 2 s deadline over connect, write and read together, and a
4 KB ceiling on the reply (`src/devhttp.rs`, hand-written over `std::net` rather than an
HTTP client crate); capped jittered backoff; on a blocking thread, never a render one.
**A browser never triggers a read** - it gets the studio's cached copy from this
server's own `/api/v1/status`.

A panel with no HTTP server is normal - older firmware, or the portable profile pointed
at `screeny-sim --no-http`. The studio says so once, falls back to UDP telemetry alone,
asks again every two minutes in case somebody updates the firmware, and never mentions
it in `/healthz`. `--no-device-http` turns the whole thing off; `--device-http-port`
points it somewhere other than 80, which is how a simulator is talked to.

**The SSID is in that payload.** It belongs on the owner's page and in this server's
`/api/v1/status`, and nowhere else: `DeviceFacts` has a hand-written `Debug` that
redacts it, and nothing about a device is written to the state file, because these are
live facts and not state. `tests/ssid.rs` checks both.

### What a panel costs the network (card 164)

The author's question: *"how much network traffic are we sending, and receiving?"*
Every device on `/api/v1/status` carries `traffic`, and the Panel screen says it in
three lines under Link:

```
Network   36.4 KB/s out · 0.3 KB/s in
By path   frames 36.1 · control 0.1 · http 0.2 KB/s out, averaged over 5 s
Sent      2.1 GB out · 4.3 MB in since the studio started
```

**Three paths, because they are three conversations**, each counted in bytes and
packets, each way:

| | | |
|---|---|---|
| `frames` | UDP, the frame port | the stream out, and the `TELEMETRY` and `BUSY` that come back along it (spec 6.4). Nearly all of it: ~30 datagrams a second of 0.4-1.4 KB. |
| `control` | UDP, the control port | the telemetry poll, brightness, identify, rename, reboot, `GET_INFO`, and the handshake at the head of **every** link session - so a panel that keeps reconnecting shows up here. Retries are counted: a request to a panel that is off is four datagrams. |
| `http` | TCP, the panel's port 80 | `GET /api/v1/status` every ten seconds (card 180). Bytes written and bytes read. |

**The overhead rule.** The per-path counters are **payload**: what `send` was handed,
what `recv` returned, what was written to and read from the socket. The `total` and
every rate add **28 bytes per datagram** (IPv4 20 + UDP 8) to the two UDP paths, so the
figure means something on the wire, and add **nothing** to the HTTP path - TCP's
retransmissions, its ACKs and its handshake are invisible from user space and are not
guessed at.

**What is not counted**, deliberately: the mDNS browse and the broadcast probe (card
141). Neither is traffic *with a panel* - a browse is multicast to nobody in particular
and a probe is one datagram to the subnet that every panel answers - so booking either
against a device would be inventing a number. Nor are the frames sent to *browsers*:
those are under `sockets`, where they have been since card 120.

**The rate is worked out in one place**, on the supervisor's own tick, as an
exponentially weighted average with a five-second time constant. So every browser reads
the same number, the page does no arithmetic (`tests/ui.rs` holds it to that), the state
is a handful of `u64` per device, and a panel that goes away decays to zero rather than
freezing at what it was last doing. **KB is 1000 bytes** here, which is what a network is
measured in, and is deliberately not what `kb()`/`size()` mean elsewhere on the page.

It costs the frame path nothing measurable: the sender adds two `u64` where it is already
incrementing `frames_sent`, the link banks the difference (so a rebuilt link adds to the
total instead of resetting it), and the studio reads the link once a second from the
supervisor. No lock on the send path, and nothing logged per packet.

### What counts as trouble, and what only looks like it (card 195)

**The thresholds are measured on the real device**, across
several builds, and they live in `devices.rs` beside their reasoning - the page carries
no copy of any of them and `tests/ui.rs` keeps it that way. Free stack has two levels,
because it is a high-water mark that only ever falls and interrupts eat it 256 bytes at
a time: `STACK_WARN` 8192 (healthy 0.4.3 reads 17-20 KB) and `STACK_FAULT` 4096 (the
0.4.0 build that worried them read 4-5 KB). The heap's one line, `HIGH_HEAP` 85%, is a
fault: steady state is 51% and the worst instant they measured, with the setup AP up, is
60%. `low_stack` is still on the wire and now means the fault level - `/api/v1/status`
is additive.

**"A reboot the studio did not ask for."** The chip cannot tell a panic from any other
software reset, so `reset_reason: software` covers our own `REBOOT`, a reflash *and* a
crash. The studio therefore asks the only question it can answer: it writes down its own
ask (the reboot control, `POST /api/v1/device/reboot`, before the request goes out) and
consumes it when the panel comes back with a new `boot_id` - one ask, one reboot, inside
`REBOOT_ASK_WINDOW` (two minutes, the status poller's own backoff cap and so the longest
it can go between reads). A `boot_id` change with no ask behind it counts in
`unasked_reboots`, and the page says so quietly: *N the studio did not ask for*, with
"a crash and a reflash look the same from here" under it. **Not a fault tone, and never
in `/healthz`** - a panel that may have crashed is not a sick server. When the firmware
reports panics, `panic` becomes a reason of its own and this gets simpler.

## The API

Everything the screens do is one of these, under `/api/v1`. Reads are `GET`, changes
are `POST` with a JSON body. A failed change is a 400 with `{"error": "..."}`; an unknown
device or panel is a 404; a device that is known but cannot be reached right now - or,
card 350, a panel that is **idle** and so has no picture to change - is a **409**, which
is a fact about the panel and not a fault in the server.

### Which panel (card 350)

**Every route that acts on "the picture" or "the panel" takes `panel`** - a device id -
as a field of the JSON body or as `?panel=` in the query string (the body wins). Without
one it means **the first panel**: the first one adopted, or the unbound stand-in when
there is none. So a page or a script written before there were several panels keeps
working, and keeps changing the panel it always changed. Channels are never addressed
directly: a client always says which panel it means, and a picture route acts on that
panel's channel - so an edit reaches every panel on the channel, which is what
`shared_with` in the state is for.

### What is playing (card 105's routes; card 170 pointed them at the panel)

These are unchanged in name and shape, plus `panel`. Every "the new state" below is that
panel's `StudioState`: `patch`, `seed`, `params` (every parameter at its effective value),
`output`, `on`, `device`, `setting`, `settings`, `modified`, `fps`, `paused`/`speed`
(retired, always `false`/`1.0`) and, since card 350, `channel` (its id, `null` when the
panel is idle - `patch` is then empty) and `shared_with` (the other panels' device ids on
that channel, in the order they joined).

| route | body | answer |
|---|---|---|
| `GET /bootstrap?panel=` | | every patch and its parameters (a parameter that is a list of named stops carries `choices`; a 0/1 one carries `switch`), which patches need a GPU (`needs_gpu`), which are worth asking "another one like this" of (`seeded`, card 151), the payload budget, the panel's state, and the adapter outcome (`gpu`) |
| `GET /frame?panel=` | | one frame packet: 52-byte header + 64x32 sRGB = 6196 bytes - what that panel was last sent |
| `GET /patch_playing?panel=` | | what a composing patch is performing, or `null` |
| `GET /panel_status?panel=` | | the panel's link, or `null` |
| `POST /set_patch` | `{id, panel?}` | a patch on its Default, by the three rules; the patch the panel is already on changes nothing |
| `POST /set_picture` | `{patch, setting?, panel?}` | card 350: a patch on one of its named settings (absent: Default), by the three rules |
| `POST /set_param` | `{id, value, panel?}` | an edit of the panel's channel |
| `POST /reset_params` | `{panel?}` | likewise |
| `POST /set_seed` | `{seed, panel?}` (`null` = a new one) | likewise |
| `POST /restart` | `{panel?}` | likewise |
| `POST /set_output` | `{output, panel?}` | the panel's own output stage - not the channel's |
| `POST /same_as` | `{as, panel?}` | card 350: put the panel on `as`'s channel, tweaks and all; 409 if `as` is idle |
| `POST /detach` | `{panel?}` | card 350: a copy of the channel for this panel alone (same patch, setting and working copy, its own clock from then on); a panel already alone is left as it is |
| `POST /set_playback` | anything | **retired** (card 302): speed and pause are not settings any more. Still a 200 with the whole state, plus `ignored` saying it changed nothing (`api::PLAYBACK_RETIRED`). Card 161 had already removed `fps` from the same body the same way |
| `POST /patch_act` | `{action, panel?}` | what it is performing; `device` is still taken for `panel` (card 140) |
| `POST /set_panel` | `{on, to?, panel?}` | `{on, device, label, panel, state}`; see below |
| `POST /settings/load\|save\|rename\|delete` | `+ panel?` | the patch's named settings; see above |
| `GET /panels` | | card 350: the overview, `{"panels": [PanelSummary...]}`, first first; see below |
| `GET /home_assistant`, `POST /home_assistant/set\|forget` | | Home Assistant (card 311); see above |
| `GET /ws?panel=` | | the frame socket; see below |

A picture pick (`set_patch`, `set_picture`, `settings/load`, `player/set` with a patch or
a setting) is **card 350's three rules**: join a channel that already shows exactly that
picture unmodified; else, alone on its channel, change it (the 2 s fade); else a new
channel of its own. A panel that moves between channels fades over 2 s. An idle panel
given a picture this way gets a channel (joining one, or a new one) and starts streaming
if its output is on.

A `PanelSummary` - one card of the overview (`panels::PanelSummary`):

```jsonc
{
  "device": "c0ffee",           // what every `panel` takes; "" for the unbound stand-in
  "name": "Kitchen",            // the name set here, else the device's own, else its instance
  "unbound": false,             // true for the stand-in of a studio with no panel yet
  "first": true,                // what a route without `panel` means
  "on": true,                   // panel output
  "brightness": 96,             // the policy, 0-255, or null
  "link": "up",                 // "idle" (no picture), "off" (output off), "none" (stand-in),
                                //   or the link's own: "up", "connecting", "waiting", "closed"
  "connected": true,            // only while "up"
  "channel": 3,                 // null when idle
  "picture": { "patch": "metaballs", "patch_name": "Metaballs",
               "setting": "Lava", "modified": false },   // null when idle
  "shared_with": ["d00d1e"]     // the other panels on the same channel
}
```

**The old names still work on the way in** (card 150). `POST /set_piece`,
`POST /set_settings`, `GET /piece_playing` and `POST /piece_act` are the same handlers
under their old paths, and a body may still call the patch `piece` (`set_piece` also
takes it as `piece` or `patch`, not only as `id`) and the output block `settings`, here
and on `POST /player/set`. **Answers use the new names only**, so there is one
vocabulary coming back.

**`set_panel` is how a script borrows the panel**, and the two bodies that matter are:

```sh
curl -s -X POST -H 'content-type: application/json' \
     -d '{"on":false}' localhost:8787/api/v1/set_panel
#  -> {"on":false,"panel":null,...}   FINAL is sent to every panel; each goes to its own
#     idle screen and stops receiving frames. The page carries on showing the picture.

curl -s -X POST -H 'content-type: application/json' \
     -d '{"on":true,"to":"screeny-c0ffee"}' localhost:8787/api/v1/set_panel
#  -> {"on":true,"device":"c0ffee","panel":{...},...}
```

Off is off for **every** panel, not only the first; `{"on":false,"panel":ID}` lets just
that one go (card 350). The answer says what happened rather than `null`, because a 200
that means "I have let it go" and a 200 that means "I am still streaming to it at 30 fps"
must not look the same. (They did, until card 170: a firmware conformance suite ran
against a panel it believed it had borrowed.)

`to` is a device id, an mDNS instance name (`screeny-c0ffee`), a host name or an address
(`192.168.1.50`, `127.0.0.1:49374`); one this studio has not heard of is added, exactly
as `POST /devices/add` would. It is looked up in the background, so attaching answers at
once whether or not the panel is there. A panel that is idle is given a picture: a studio
still on its unbound stand-in hands it that (the stand-in is renamed onto the device, so
nothing restarts); otherwise the default patch on Default, by the three rules. Card 170's
`to` also moved "the page" onto that panel; since card 350 the page chooses its panel
itself (`?panel=`) and `to` drives the device and nothing else. Every change is persisted.

### Panels, devices and health (card 106)

| route | body | answer |
|---|---|---|
| `GET /status` | | everything: health, the state file, discovery, the graphics adapter or why there is none (`gpu`, card 145 - never a reason for a 503), what the first panel is showing (still keyed `preview`, for scripts written against card 106), the overview (`panels`, card 350), every device |
| `GET /devices` | | the device half of `/status` on its own. Each device's `player` is its panel and its panel's picture - card 106's shape, plus `channel`, `setting`, `modified` and `shared_with`; `health.ticks` is its channel's renders, the same number on every panel sharing it. `attached` / `player.focused` mean "the first panel" |
| `POST /devices/add` | `{to, name?, play?}` | `{id}` - a new panel, by name or address, **idle** unless `play` (then as `set_panel {"on":true,"to":...}`) |
| `POST /devices/add` | `{to, device}` | `{id, moved}` - **this** panel is somewhere else now |
| `POST /devices/forget` | `{device}` | the panel goes with it, and its channel if nobody else is on it |
| `POST /devices/refresh` | `{}` | ask every unresolved panel who it is, now |
| `POST /player/set` | `{device, on?, patch?, setting?, seed?, param?, reset_params?, restart?, output?, brightness?}` | the panel's `player` view. A patch or a setting is a picture pick (three rules; one that also carries a seed, a parameter or a reset never *joins* another panel's channel, since the edit would land on them); a seed, a parameter, a reset or a restart edits the channel it is then on (an idle panel is first given the default patch); `on`, `output` and `brightness` are the panel's. `fps?` (card 161), `paused?` and `speed?` (card 302) were here; still accepted, still ignored |
| `POST /device/brightness` | `{device, level}` | `{asked, applied}` - and it becomes the panel's policy |
| `POST /device/identify` | `{device, ms?}` | |
| `POST /device/name` | `{device, name}` | renames it here, and on the device when it can be reached |
| `POST /device/reboot` | `{device, confirm}` | `confirm: true` is required |
| `POST /device/stats` | `{device}` | telemetry, read now rather than from the poll |

### The frame socket, `GET /ws`

A socket is about **one panel**: `?panel=<device id>`, or - without it - the first panel,
which it keeps following (a studio whose unbound stand-in gives way to a real panel moves
the socket along within a heartbeat, with a fresh `state`). What comes out of it:

- **binary**: one frame packet, as `GET /api/v1/frame?panel=` returns - that panel's, and
  the same bytes it is being sent;
- `{"type":"state","rev":N,"from":"<client>"|null,"panel":"<device id>","state":{...}}`
  whenever anything changes, so several browsers stay in step - the socket's own panel's
  state, read at the moment it is sent (an edit to a shared channel changes every panel on
  it, so every socket hears of every change and speaks for its own panel);
- `{"type":"status","playing":...,"panel":...}` twice a second, for that panel;
- `{"type":"panels","panels":[PanelSummary...]}` - the overview, when the socket opens and
  whenever it changes (a picture, a link coming up or going, output, brightness).
  `?overview=false` asks for none;
- `{"type":"error","error":"no panel `x`"}` and a close, for a socket that named a panel
  the studio does not have. A socket whose panel is forgotten is closed.

**One socket per panel is cheap on purpose**, and it is how the overview's live
thumbnails are meant to be fed (card 351): `?panel=X&fps=4&repeat=false&overview=false`
is at most four 6 KB frames a second, fewer while the picture holds still, one state
message per change and two small heartbeats a second. Nothing is multiplexed: every
socket is the same simple thing, and pacing, the watcher count and the stall timeout stay
per panel. The page's own socket, without `panel`, carries the overview.

A browser identifies itself with an `X-Studio-Client` header on changes and
`?client=<id>` on the socket; the server does not echo a browser its own change.

**State is paced too** (card 196), because dragging a slider is sixty
`set_param` a second and each one put a whole `StudioState` - 416 bytes - on
every *other* socket: 25 KB/s of JSON for one control moving, and on a hidden
tab, which asks for no pictures at all, everything that tab cost. A socket is
now sent at most one state message every 50 ms - 20 a second - and the one it
is sent is always the newest. The first change after a quiet moment still goes
out at once, so a patch picked or a switch flipped is as immediate as it ever
was; only a burst is thinned, and the last change of a burst is **held rather
than dropped**, so the value a drag ended on always arrives and no browser is
left resting on a stale one. Measured: 60 messages/s and 24.9 KB/s become 20/s
and 8.0 KB/s, with a single change crossing in about 4 ms.

**And one goes in** (card 120). A frame packet is 6196 bytes, so every frame a second
is 6.2 KB/s per tab; a socket says how many of them it wants, on the way in with
`?fps=&repeat=` or at any time with

```json
{"type":"preview","fps":30,"repeat":false}
```

- `fps` - the most frame packets a second. **`0` means none**: what the page sends when
  its tab is hidden, where every frame would be received and thrown away. State changes
  and the heartbeat carry on, so a hidden tab stays correct for about half a kilobyte a
  second. Default 30, which since card 161 is every frame there is; more than that is
  taken as "everything".
- `repeat` - whether to send a picture identical to the one this socket was last sent.
  `false` skips it, except once a second so the header's counters keep moving; a held
  clock face is the same 6196 bytes for fifteen seconds at a time. Default `true`.

The page asks for 30 while visible, 10 when `navigator.connection` says the link is slow
or the owner has asked for less data, and 0 when hidden. Anything else a browser sends is
ignored, so an old page and a new server understand each other in both directions.

Asking for frames is also how the server knows somebody is watching: a channel none of
whose panels is connected, and none of whose panels anybody is **looking at**, drops to
5 fps rather than rendering 30 for a month - a hidden tab is not a watcher, or a phone
left on the page in a pocket would hold a core open. Nothing a browser does can slow a
channel down: every frame cell has one slot, the render loop never waits for a reader,
and pacing is done by dropping a frame where it stands rather than by holding one.

`GET /status` says what all this is costing, under `sockets`: `open`, `watching`,
`frames_sent`, `bytes_sent`, across every panel's sockets. That is what the *browsers*
cost; what the *panels* cost is per device under `traffic` (card 164, above), and the two
are deliberately not added together - one is a LAN and the other is a Wi-Fi link with a
64x32 panel on the end of it.

**Brightness** is a policy, not a one-off: it is re-applied whenever the link comes back,
and whenever the panel's own telemetry disagrees with what it last said it applied - a
panel that power-cycles faster than UDP notices comes back at full brightness otherwise.
The answer is what the device *applied*, which its own cap may make lower than what was
asked; the policy is then lowered to match, so the studio does not ask for something the
panel will not give. The page's slider maximum is that cap, and its lowest non-zero
stop is 6, because values 1..=5 light nothing on this firmware (card 136).

The slider itself **holds what was chosen** (card 126): the panel's telemetry is a
periodic poll, so for a while after a change it still carries the old brightness, and
painting it would bounce the slider back and then forward again. `common.js` holds the
true applied value - from the change's own reply, so a floor or a cap shows correctly -
until a fresher reading agrees or a few seconds pass, whichever comes first, and only
then goes back to following telemetry. That bound is also the answer to "how fast does
a change made elsewhere (a second browser, another API caller) show up": within it,
because every change updates the one server-side policy every browser's next state
message carries.

## Editing the UI

`ui/` is eight static files and no build step:

```
index.html      picture.js    ┐
panel.html      panel.js      ├─ common.js   style.css
settings.html   settings.js   ┘
```

Three documents, one per screen, and the scripts are plain ES modules: each screen loads
its own, which `import`s the shared one. `common.js` is the socket, the status poll,
the notice line, the formatting, the small control bindings, the brightness control and
the one judgement of what the panel is doing - and it **reaches for no element by id
except `#notice`**, which every screen has; everything else is handed the element it
works on. Separate documents rather than one document with several views because
"nothing about devices is on the Picture screen" is then a fact about the file, and
because reload, the back button and a bookmark are the browser's job rather than a
`popstate` handler's.

They are `include_bytes!`d into the binary, so `cargo run` always serves what is in the
tree; **`--ui-dir` serves them off disk** for a reload-to-see-it loop, which is what to
use while editing. A new file has to be listed in `src/ui.rs`, and so does a new screen's
tidy URL.

**One nav** (card 301), the same markup on every screen, at the very top of it: real
`<a href>`s to `/`, `/panel` and `/settings`, the current one marked with
`aria-current="page"` - real navigation, so reload and the back button stay the
browser's job, rather than tabs that swap what one document shows. Card 311 gave the
third place to Settings; `/schedule` redirects (307) to `/` for an old bookmark.

Each screen is a **scrolling column by default** - the Picture screen is picture, view,
now playing, parameters... - and splits into two columns only above 1100 px, where
there is room. Doing it the other way round is what used to put the walnut frame on
top of the controls at around 600 px. Checked at 390 and 1400 px in a browser for both
screens (card 198's Log), and at 390, 600, 900 and 1400 in card 170's; the screenshots
are in those Logs and, for the controls below, in cards 145/163/171-173's. The Panel
screen's two-column bench is floated, not gridded (card 303): a CSS grid's row tracks
are shared across both columns, which used to strand the shorter side's second section
below all of the longer side's height; a float only answers to what is above it in the
same column. The Settings screen (card 311) is a single narrow sheet, capped at 640 px,
and never earns either layout.

**A control's shape comes from what it controls** (card 163). The page builds each
parameter from its `ParamSpec`: an ordinary number is a slider, a spec with `choices` is
a segmented control (three stops or fewer, which fit across the inspector at 390 px) or a
`<select>` (more than three), and a spec with `switch` is a switch. Nothing about the
value changes - it is an `f32` set with `set_param` either way - so a patch asks for the
control it wants by how it declares the parameter, and never by putting a key in a label.

**Nothing on any screen may say something that is not so.** The output switch says
what it really does when there is no panel (181); the Panel screen says whether the
studio is even looking for panels (173); "Reconnects" is a panel-lifetime count that
survives the link being rebuilt (171); a patch that needs a graphics adapter there is
none for is struck through with the reason rather than offered and then black (145);
and the brightness slider says out loud that it is the panel's own brightness, which is
why the picture on screen does not change with it - in percent of full light since card
312, 0 to 100 in the panel's own 4 % steps, the same number Home Assistant's slider shows. Card 151 took the **seed's number**
off every screen for the same reason: it was a readout that told a person nothing they
could act on. Card 301 went further and took the seed's own control off the page too -
the quiet *Another* button nobody understood - along with Speed and the pause/restart
controls, because a rate somebody could vary and a start/stop that (the owner's word)
baffled were not earning their place either.

**The settings control** heads the Parameters section, above what it holds: the name, a
mark when it has been moved since, the list to load from with Default first, and Save /
Save as... / Rename / Delete / Revert. On Default, `Save` *is* `Save as...` and Rename and
Delete are disabled, so the read-only one says so by shape rather than by refusing
afterwards. Naming and confirming are **inline** - no `prompt()`, no `confirm()`: those
cannot be driven by the browser tooling, cannot be styled and stop the page - and a
refusal appears beside the control rather than on the page's shared notice line.

No framework, no bundler, no CDN: the box this runs on has no promise of internet, and a
test asserts that no file of any screen reaches outside it.

## Tests

`cargo test -p screeny-studio`. Nothing touches the bench device or the LAN: every
target is an explicit `127.0.0.1`, discovery is off in `Config::default()` on purpose,
and `state_dir` is `None` there too so a test cannot leave a file behind.

| file | what it pins |
|---|---|
| `tests/api.rs` | the page's routes, the frame socket, two browsers in step, the heartbeat, a frame packet's shape, and a studio with no panel at all |
| `tests/preview.rs` | card 120: a hidden tab is sent no pictures, keeps its heartbeat and comes straight back; a socket is paced to what it asked for, and asking for more than is rendered means "everything" (161); an unchanged picture is not sent again; **a hidden tab is not a watcher**, so a studio with no panel and only hidden tabs idles at 5 fps; and `sockets` on `/status` counts at least what a browser received |
| `tests/pacing.rs` | card 196: with two browsers open and one of them dragging a slider at 60 Hz, the other is sent a bounded number of state messages a second (measured 19.3-19.7/s and 8.0 KB/s, against 60.0/s and 24.9 KB/s before the card) and the dragging one is sent none; a single deliberate change still crosses in a few milliseconds; and the value a drag **ended on** always arrives, within one gap of the drag stopping |
| `tests/panel.rs` | what the browser draws is what `screeny-sim` shows, byte for byte; a stalled browser holding up neither a channel nor the link; **`set_panel` really hands the panel over and takes it back**, asserted on what the device sees; and a panel stopped and started twice reading **2 reconnects**, across a link rebuild (171) |
| `tests/channels.rs` | **card 350's acceptance**, against two `screeny-sim` panels on loopback: the same picture on both is one channel - one render per tick handed to both (counted: renders against each panel's presents) and the same picture on both devices; different pictures are two channels and the sims disagree; **Same as** puts them back in sync, **Detach** separates an edit; a route without `panel` is the first panel's and `set_panel {"on":false}` lets both go; a socket scoped with `?panel=&fps=4` is sent that panel's state at that rate, one without `panel` is the first's and carries the overview, one naming no panel is told so; a device the registry learns about becomes an idle panel and the stand-in goes |
| `tests/fleet.rs` | devices, panels, containment, health, the device controls - and **the card's acceptance**: kill the simulator, the server, or both in either order, and the panel comes back playing what it was playing |
| `tests/soak.rs` | a bounded soak at accelerated time: frame loss, the panel going away, the panel moving, a run of changes; flat memory, nothing dead, recovery after every fault. `SCREENY_SOAK_SECS` lengthens it |
| `tests/device_status.rs` | card 180: with the panel's HTTP API on, the page has heap, free stack, slot and WiFi beside the UDP telemetry; with it off, nothing complains and `/healthz` stays 200; a simulator restarted on the same ports is counted as **one** reboot, from `boot_id`; **at most one connection open to a device at a time**, measured by a server that counts them; and a reply that never ends is refused rather than read |
| `tests/device_health.rs` | card 195: the rows nobody ever sees, driven on a **running** simulator with `SimHandle::set_health` - a stack of 6000 warns and 3000 faults, a 90% heap faults and the 60% measured with the setup AP up does not, a brownout / store errors / a `pending_verify` slot stand out, a reboot asked for through the studio's own control is not counted and the ask is used up, one taken behind its back is, and the real panel's readings show nothing at all |
| `tests/ssid.rs` | the network name is on `/api/v1/status`, where the page needs it, and in neither the studio's log (checked by running the real binary as a subprocess and reading its stderr) nor `state.json` |
| `tests/traffic.rs` | card 164: against a simulator, the KB/s out is `frames sent x mean frame bytes + 28 B a datagram` (measured 1.4% out); the http counters move both ways on every poll and the control ones when somebody presses Identify or moves brightness; the totals only grow, **including across the panel being taken away and given back**, which rebuilds the link and resets its own counters; two reads inside one tick are identical, which is what "one rate, every browser" means; and a panel that is away counts nothing |
| `tests/ui.rs` | **all three screens** and their eight files are served, `/panel` and `/panel.js` (and `/settings`/`/settings.js`) are not the same thing, `/dashboard` and `/schedule` redirect, every element each screen's script reaches for exists in that screen (and every element `common.js` reaches for exists in **all three**), every route they call exists, each screen's narrow layout stays the default, each screen's sections stack in the order they should (301), and **one nav with the current screen marked** is on every one of them (301, 311) - and, since the truth-telling cards, that the split holds (198: nothing about devices on the Picture screen, no canvas and no frames asked for on the Panel or Settings screens; 301: brightness, the panel model and the limiter bound once, on the Panel screen only now), that the Panel screen can say whether discovery is on (173), that the adapter outcome is on both routes and is never a 503 (145), that **no control on any screen offers a frame rate** and an old body that still carries one is accepted and ignored (161, which removed card 172's rate slider), that **no slider declares stops any more** and the drawing mechanism went with Speed, its last caller (183, 197, retired by 301), that a parameter with named stops carries them (163), that **the seed is not a control anywhere** (151, 301), and - run under `node` when the machine has one, skipped cleanly when it does not - that the brightness slider's hold-then-release rule releases the moment a reading agrees and otherwise at its deadline, never later, never earlier (126) |
| `tests/memory.rs` | cards 165 and 350: a patch picked afresh is on its Default and **a named setting brings a tuning back**, from any patch, on the page, on a panel and in a second browser; Reset; **a fresh process on the same state directory is showing what it was, tuning and all, and has every setting - including one for a patch that is not showing**; one library of settings for two panels (and the second joins the first's channel); a hand-edited file with garbage values in a setting; a v1 file |
| `tests/ui.rs`, `src/state.rs` | card 151: save / load / rename / delete over the API with the list, the name and the mark travelling in the state; every refusal a 400 in words; a setting older than the patch; Default read-only in any spelling; the name rules and the 64 bound; a realistic v4 file migrated to v5 with its speed carried and the v4 file kept; a v5 file that does **not** run the migration again; a hand-edited `settings` block where every way of being wrong costs that value alone; and the seed's number gone from both screens |
| `tests/ha_mqtt.rs` | cards 308-311 and 352 (one HA device per panel, two sims), **ignored by default** - needs a broker (`SCREENY_TEST_MQTT=HOST:PORT`, see above): the picture select and its options following the named settings, the brightness light and the percent slider, the settings routes (`GET`/`POST /home_assistant/set\|forget`), reconnecting after a broker restart and after HA's own restart, and forgetting clearing every retained topic |
| `src/*` unit tests | the state file's failure modes - including card 310's `note_retired_modes`, said once when a v6 file's modes and timetable are dropped - the registry's keying, a channel's edits and its fade, **card 350's topology** (`src/panels.rs`: adoption idle, the three picking rules, Same as, Detach, a setting renamed or deleted under every channel on it, a channel dropped once nothing is on it or fading away from it, one render handed to two panels and two channels apart, the stand-in taken over by the first panel named), a panel's fade from one channel to another (`src/panel.rs`), **v7 -> v8** with the v7 file kept and v8 files whose panels and channels disagree, the argument and environment precedence, and (`src/ha/`) the discovery payload, the topic layout, and command parsing against fixed snapshots |
