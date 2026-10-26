# Changelog

## 0.1.8

- **flame clock** is now called **Flavortime**. Its settings start over at
  their defaults on a panel that was showing it.

## 0.1.7

- New patch, **flame clock**: hearth's fire telling the time. Once a minute,
  around the minute turning, the fire lights the time - the digits are fuel,
  and catch where the flames reach them. At the minute a jet of flame shoots
  up under the digits that change, the old ones fall away and the new ones
  catch in their place; then the time collapses into the fire under a surge
  and the fire burns on its own until the next minute. In 12-hour time the
  hour turning over (9:59 to 10:00, 12:59 to 1:00) blows the old time apart
  and launches the new one from below. Settings: how long the time shows
  before and after the minute, where it stands, how tall and how heavy the
  digits are, and the fire's size, gusts, embers, smoke, draft, speed and
  colour. Needs a real GPU, as hearth does.

## 0.1.6

- **hearth** burns wood now: flames rise off a small pile of logs, wrapping
  round them and burning hardest in the gaps, centred so the fire reaches as
  far left as right. New settings: **speed** (1 is the slow, grand fire; up
  to 18, about real speed), **wood** (how patchily the logs give off gas),
  **expansion** (how hard burning gas pushes up) and **headroom** (how high
  the stove's ceiling sits). A fast fire is simulated more coarsely so it
  keeps a smooth frame rate, and the panel's overall brightness is held
  steady so a fast fire never flashes.

## 0.1.5

- New patch, **hearth**: a fire in a wood-stove firebox, simulated in 3D on the
  GPU. Flames, grey smoke and sparks rise from a bed just below the frame, with
  thermal gusts, logs that settle, and a chimney draft at the top. Sparks
  occasionally bounce off the sides and the ceiling. Size, gusts, embers, smoke
  and draft are settings, eased so Home Assistant can drive them. Needs a real
  GPU, so it is not offered on a machine without one (such as a Raspberry Pi).

## 0.1.4

- Finds a panel again after the router gives it a new address, even when the
  panel is still announcing its old one (it follows the panel's own answer to
  the network-wide probe).
- Carries firmware 0.11.3: the panel announces its new address as soon as its
  lease moves, and a crash record names the CPU exception.
- The clock patch no longer writes two log lines a minute.

## 0.1.3

- Carries firmware 0.11.2: the panel's own web page is written for owners
  and can install a firmware file.

## 0.1.2

- Carries firmware 0.11.1 (the web flasher's WiFi step works) and offers it
  to panels on 0.11.0.

## 0.1.1

- Each panel's Home Assistant device is named after the panel, and shows the
  panel's own firmware version. The firmware update entity no longer shows
  Unknown.
- A device left behind by another Studio on the same broker is cleared.
- Settings can be exported from one Studio and imported into another.
- The Studio carries firmware 0.11.0 and offers it to older panels.

## 0.1.0

- First version of the app: the Studio behind Home Assistant's ingress, with
  its MQTT broker taken from Home Assistant.
