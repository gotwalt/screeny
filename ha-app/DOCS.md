# Screeny

Generative art for a Tidbyt LED panel (Gen 1, 64x32), and the panel as a device in
Home Assistant. This app runs the Screeny Studio.

## Install

1. In Home Assistant open **Settings > Apps > App store**, the three-dot menu,
   **Repositories**, and add `https://github.com/gotwalt/screeny`.
2. Install **Screeny** and start it. Leave **Start on boot** and **Watchdog** on.

## Open it

**Screeny** appears in Home Assistant's sidebar. That is the Studio: what is
playing, the panels it knows, and the settings. It is shown through Home Assistant,
so Home Assistant's login is what protects it.

By default the Studio does **not** listen on your network at all. It is reachable
only from that sidebar entry (Home Assistant's "ingress").

## Your panel

The Studio finds Tidbyts running the Screeny firmware on your network by itself, and
a new panel joins Channel 1. Putting the firmware on a Tidbyt for the first time is
done from your browser: the flasher is at *(link to come: card 363)*.

## MQTT: nothing to set up

If Home Assistant has an MQTT broker (the **Mosquitto broker** app, usually), the
Studio is given its address and login by Home Assistant and shows up as a device
with the picture, patch, channel and brightness as entities. Nothing is typed in the
Studio's **Settings** screen.

- If the broker is not installed yet, the Studio checks again every twenty seconds.
- A broker you type on the **Settings** screen always wins over the one Home
  Assistant offers, including switching the connection off.
- The broker's password is never shown in the Studio, in its API or in its log.

## Options

| option | default | what it does |
|---|---|---|
| **Direct access** | off | Also serve the Studio on your network, on the port below, **with no login**. Anyone who can reach the port can change what your panels show. Leave it off unless you need the Studio from something that cannot use Home Assistant's sidebar. Never publish the port beyond your home network. |
| **Direct access port** | 8787 | The port for direct access. Not 8099, which Home Assistant's own entry uses. |

Changing an option needs the app restarted.

## No graphics card? That is normal

Home Assistant OS on a Raspberry Pi gives apps no GPU. Everything still plays except
the two patches that need real graphics hardware, **overland** and **ghosts**, which
are marked unavailable on the Picture screen. A machine with a GPU that Home
Assistant exposes to apps plays them all.

## Time

The Studio's clock patches show Home Assistant's time zone, which the Supervisor
passes to every app.

## Problems

- The sidebar entry opens a blank page: restart the app and read its **Log**; the
  Studio says on its first lines how it is listening.
- "Home Assistant offers no MQTT broker yet" in the log: install and start the
  Mosquitto broker app, or set a broker on the Studio's Settings screen.
- No panel appears: the panel and Home Assistant must be on the same network
  segment (mDNS does not cross routers).
