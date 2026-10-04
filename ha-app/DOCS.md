# Screeny

Generative art for a Tidbyt LED panel (Gen 1, 64x32), and the panel as a device in
Home Assistant. This app runs the Screeny Studio.

This page walks from nothing to a panel in Home Assistant. Screenshots are
marked where they go.

## What you need

- A **Tidbyt Gen 1** (the wooden one). Gen 2 is untested.
- A **USB-C data cable**. Some charging cables carry power only and the Tidbyt will
  not show up in the port list; try another.
- A computer with **Chrome, Edge or Opera** (desktop). Flashing from the browser uses
  Web Serial, which Firefox, Safari and phones do not have.
- **Home Assistant OS or Supervised** (apps need the Supervisor) on an `aarch64` or
  `amd64` machine. Container and Core installs have no apps.
- Ideally the **Mosquitto broker** app, so the panel shows up as a Home Assistant
  device. Without it the app still works.
- Your WiFi name and password (2.4 GHz). <!-- verify: 2.4 GHz only; the flasher page says so -->

## 1. Put the firmware on the Tidbyt

Open the web flasher at <https://gotwalt.github.io/screeny/> in desktop Chrome, Edge
or Opera, plug the Tidbyt into the computer, and click **Install**.

<!-- screenshot: flasher-install -->

1. Pick the port that appears (CP2102 or USB Serial).
2. The dialog installs the firmware. On a Tidbyt still running its original firmware
   it erases the chip first. This takes about three minutes; do not unplug it.
3. When it finishes, the dialog offers to connect the Tidbyt to WiFi. Choose your
   network and type the password. They go to the Tidbyt over the cable and nowhere
   else.

<!-- screenshot: flasher-wifi -->

The panel then joins your network. If the dialog says it failed to initialize, unplug
and replug the cable and try again. <!-- verify: WiFi step needs a firmware release with Improv (0.11.0); check the flasher offers it on the published release -->

To go back to Tidbyt's own firmware, see [help.tidbyt.com](https://help.tidbyt.com).

## 2. Add the app to Home Assistant

1. Open **Settings > Apps > App store**. <!-- verify: menu is called Apps or Add-ons depending on the HA version -->
2. Open the three-dot menu, choose **Repositories**, and add
   `https://github.com/gotwalt/screeny`.

   <!-- screenshot: add-repository -->

3. Reload the store, open **Screeny** and click **Install**.

   <!-- screenshot: app-page -->

4. Start it. Leave **Start on boot** on. <!-- verify: Watchdog was removed from the app (the image has a HEALTHCHECK); the old text said to leave it on -->
5. **Screeny** appears in Home Assistant's sidebar. Open it.

   <!-- screenshot: studio-in-sidebar -->

That is the Studio: what is playing, the panels it knows, and the settings. It is
shown through Home Assistant ("ingress"), so Home Assistant's login is what protects
it. By default the Studio does **not** listen on your network at all.

## 3. Your panel

The Studio finds Tidbyts running the Screeny firmware by itself (mDNS), and a new
panel joins Channel 1. It should appear on the Studio's panel list within a minute or
so. <!-- verify: time to appear -->

<!-- screenshot: studio-panel-found -->

### In Home Assistant

If Home Assistant has an MQTT broker, the Studio is given its address and login by
Home Assistant and announces itself over MQTT discovery. You type nothing in the
Studio's **Settings** screen. Home Assistant gets a device, **Screeny**, named by the
Studio's Device name setting, with these entities:

| entity | what it is |
|---|---|
| **Screeny** (light) | on/off and brightness, 0-255 |
| **Brightness** (number) | the same as a 0-100 % slider, in steps of 4 |
| **Picture** (select) | a patch on one of its named settings, for example "Overland · Dusk". Picking one changes what Channel 1 shows |
| **Patch** (sensor) | the patch now playing; its attributes carry the id, setting name and whether it was modified |
| **Channel** (select) | which channel the panel shows |
| **Panel link** (diagnostic) | whether the Studio is streaming to the panel |

<!-- screenshot: ha-device -->

A second panel is a device of its own with a Channel select, and every channel after
Channel 1 is a device with a Picture and Patch, because a channel owns the picture and
panels are members of channels.

The Studio has no timetable. Time of day is Home Assistant's business: make an
automation that sets **Picture** and **Brightness**, for example a dim clock at night.

- If the broker is not installed yet, the Studio checks again every twenty seconds.
- A broker you type on the Studio's **Settings** screen always wins over the one Home
  Assistant offers, including switching the connection off.
- The broker's password is never shown in the Studio, in its API or in its log.

## 4. Firmware updates from Home Assistant

<!-- pending card 364 -->

Each panel gets an **update** entity in Home Assistant. When the app bundles newer
firmware than the panel runs, the entity shows an update; **Install** pushes it to the
panel over WiFi. The panel keeps its WiFi settings, and rolls back by itself if the new
firmware does not start properly. The firmware an app version offers is inside the app
image, so updating the app is what makes a firmware update appear.

## Options

| option | default | what it does |
|---|---|---|
| **Direct access** | off | Also serve the Studio on your network, on the port below, **with no login**. Anyone who can reach the port can change what your panels show. Leave it off unless you need the Studio from something that cannot use Home Assistant's sidebar. Never publish the port beyond your home network. |
| **Direct access port** | 8787 | The port for direct access. Not 8099, which Home Assistant's own entry uses. |

Changing an option needs the app restarted.

## Time

The Studio's clock patches show Home Assistant's time zone, which the Supervisor
passes to every app.

## Troubleshooting

**No panel appears in the Studio.** The panel and Home Assistant must be on the same
network segment: discovery is mDNS, which does not cross routers or VLANs. The app
runs on the host's network for this reason. Check the panel has joined WiFi (it shows
a QR code and raises an open `screeny-<id>` network if it has none) and that your
router does not block multicast between WiFi and wired devices.

**The panel is in Home Assistant's app but not an entity.** The entities need an MQTT
broker. In the app's **Log**, "Home Assistant offers no MQTT broker yet" means none is
installed: install and start the Mosquitto broker app and add the MQTT integration, or
set a broker on the Studio's Settings screen. The Studio works fine without one, you
just cannot control it from Home Assistant. <!-- verify: MQTT integration must be added for entities to appear -->

**The sidebar entry opens a blank page.** Restart the app and read its **Log**; the
Studio says on its first lines how it is listening.

**No graphics card? That is normal.** Home Assistant OS on a Raspberry Pi gives apps no
GPU, so the Studio draws in software. Everything still plays except the two patches
that need real graphics hardware, **overland** and **ghosts**: they are marked
unavailable on the Picture screen and left out of Home Assistant's Picture list. A
machine with a GPU that Home Assistant exposes to apps plays them all.

**Running the Studio without Home Assistant**: see `docs/design/deployment.md` in the
[repository](https://github.com/gotwalt/screeny) (docker-compose, image
`ghcr.io/gotwalt/screeny-studio`).
