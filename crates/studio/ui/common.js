// What both of the Studio's screens are made of.
//
// There are two screens since card 198 - the **Picture** (`/`) and the
// **Panel** (`/panel`) - and one server behind them. What they share is
// everything that is not layout: the socket, the poll, the formatting, the
// small control bindings, and the one judgement of what the panel is doing.
// Each screen's own file (`picture.js`, `panel.js`) imports from here and
// touches only the elements its own page has.
//
// Plain ES modules, loaded by the browser. There is still no Node toolchain,
// no bundler and no build step: `src/ui.rs` lists the files and the browser
// does the rest.
//
// The rule this file lives by: **nothing here reaches for an element by id**
// except the notice line, which both screens have. Everything else is handed
// the element it works on, so a function cannot half-work on the screen that
// does not have it.

'use strict';

/** How often to re-read the panel's own facts. */
export const STATUS_MS = 2000;

/* Card 136: the firmware has 25 real brightness steps and values 1..=5 light
 * nothing at all while `applied` cheerfully echoes them back. So the slider's
 * lowest non-zero stop is the first value that lights the panel. Delete this
 * constant and the one snap() call below when 136 fixes the firmware. */
const BRIGHTNESS_FLOOR = 6;
export const snapBrightness = (v) => (v > 0 ? Math.max(v, BRIGHTNESS_FLOOR) : 0);

export const $ = (sel, root = document) => root.querySelector(sel);

/** Never fight an input somebody is using. */
export const busy = (el) => el === document.activeElement;

// ---------- words and numbers ----------

export const pct = (v) => `${Math.round(v * 100)}%`;
export const trim = (v, step) => v.toFixed(step >= 1 ? 0 : step >= 0.1 ? 1 : 2);
export const nf = new Intl.NumberFormat();

export function ago(seconds) {
  if (seconds === null || seconds === undefined) return 'never';
  if (seconds < 2) return 'just now';
  if (seconds < 90) return `${Math.round(seconds)} s ago`;
  if (seconds < 5400) return `${Math.round(seconds / 60)} min ago`;
  return `${Math.round(seconds / 3600)} h ago`;
}

export function duration(seconds) {
  if (seconds === null || seconds === undefined) return '–';
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (d) return `${d} d ${h} h`;
  if (h) return `${h} h ${m} min`;
  if (m) return `${m} min`;
  return `${Math.round(seconds)} s`;
}

/** A snake_case name from the device's API, as a person reads it. */
export const words = (s) => String(s).replace(/_/g, ' ');

export const kb = (bytes) => `${Math.round(bytes / 1024)} KB`;

/** A size a person reads rather than counts: KB up to a megabyte, then MB. */
export const size = (bytes) => (bytes < 1024 * 1024 ? kb(bytes) : `${(bytes / 1048576).toFixed(1)} MB`);

/* Card 164: network numbers are in powers of ten.
 *
 * `kb` and `size` above are 1024 and are about memory and buffers. A network
 * is measured in thousands - a 100 Mbit link is 100,000,000 bits - so the two
 * are deliberately different functions rather than one with a flag, because
 * getting them confused is how a page ends up 2.4% wrong and nobody notices. */

/** A rate: KB/s, one decimal - the card's own example is `36.4 KB/s out`, and
 *  a "By path" line reading `frames 40 · control 0.0` looks like two different
 *  kinds of number rather than one. `unit` false leaves the suffix off for a
 *  list of figures that share one, and that form is **always kilobytes**,
 *  whatever the size. */
export function kbs(bytesPerSecond, unit = true) {
  const k = (bytesPerSecond || 0) / 1000;
  if (unit && k >= 1000) return `${(k / 1000).toFixed(2)} MB/s`;
  return unit ? `${k.toFixed(1)} KB/s` : k.toFixed(1);
}

/** A total, in the same powers of ten: "2.1 GB" is the number that answers
 *  "what has this cost my network". */
export function netSize(bytes) {
  const b = bytes || 0;
  if (b < 1000) return `${Math.round(b)} B`;
  if (b < 1e6) return `${(b / 1e3).toFixed(1)} KB`;
  if (b < 1e9) return `${(b / 1e6).toFixed(1)} MB`;
  return `${(b / 1e9).toFixed(2)} GB`;
}

/** What the panel does when nothing is streaming, spelled out. The keys are
 *  `screeny_device_api::IdleMode`; anything else falls back to the raw name,
 *  so firmware that grows a mode says something rather than nothing. */
export const IDLE = {
  status: 'shows its status screen',
  hold_forever: 'holds the last frame',
  dim: 'dims the last frame',
  black: 'goes black',
};

/** The WiFi line. A non-null address means the link is up whatever
 *  `wifi_state` says - see `wifi_stale_failure` in devices.rs. */
export function wifiLine(f) {
  const where = f.ssid || 'no network';
  return `${where} · ${f.link_up ? `${f.rssi_dbm} dBm` : words(f.wifi_state)}`;
}

/** Fill a <dl> from [label, value, tone] triples, reusing its rows. */
export function facts(dl, rows) {
  while (dl.children.length > rows.length * 2) { dl.lastElementChild.remove(); }
  rows.forEach(([label, value, tone], i) => {
    let dt = dl.children[i * 2];
    let dd = dl.children[i * 2 + 1];
    if (!dt) { dt = document.createElement('dt'); dl.append(dt); }
    if (!dd) { dd = document.createElement('dd'); dl.append(dd); }
    if (dt.textContent !== label) dt.textContent = label;
    const text = String(value);
    if (dd.textContent !== text) dd.textContent = text;
    if (tone) { dd.dataset.tone = tone; } else { delete dd.dataset.tone; }
  });
}

// ---------- the notice line ----------

let noticeTimer = null;

/** The one line either screen says things on. Both pages carry a `#notice`;
 *  this is the only element this file knows by name. */
export function notice(message, tone) {
  const el = $('#notice');
  if (!el) return;
  el.hidden = !message;
  el.textContent = message || '';
  if (tone) { el.dataset.tone = tone; } else { delete el.dataset.tone; }
  clearTimeout(noticeTimer);
  // A transient message goes away; a lost connection does not.
  if (message && tone === 'say') { noticeTimer = setTimeout(() => { el.hidden = true; }, 6000); }
}

/** Run something that talks to the panel, and say what came of it. `after` is
 *  whatever the screen wants re-read once it is over, however it went. */
export function makeAttempt(after) {
  return async function attempt(what, fn) {
    try {
      const out = await fn();
      notice(typeof out === 'string' ? out : what, 'say');
      return out;
    } catch (e) {
      notice(`${what}: ${e.message || e}`);
      return null;
    } finally {
      after();
    }
  };
}

// ---------- talking to the server ----------

// This browser, so the server can leave our own changes out of what it pushes
// back to us: adopting them would fight with the slider still under the mouse.
export const CLIENT = crypto.randomUUID?.() ?? `c${Math.random().toString(36).slice(2)}`;

// Reads; everything else is a POST carrying its arguments as JSON.
const GETS = new Set(['bootstrap', 'frame', 'patch_playing', 'panel_status', 'status', 'devices']);

export async function invoke(cmd, args) {
  const init = GETS.has(cmd) ? {} : {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-studio-client': CLIENT },
    body: JSON.stringify(args ?? {}),
  };
  const response = await fetch(`/api/v1/${cmd}`, init);
  if (!response.ok) {
    let detail = `${response.status} ${response.statusText}`;
    try { detail = (await response.json()).error ?? detail; } catch { /* not JSON */ }
    throw new Error(detail);
  }
  if ((response.headers.get('content-type') || '').startsWith('application/json')) return response.json();
  return response.arrayBuffer();
}

// Card 120: how many frames a second to ask the server for.
//
// One frame packet is 6196 bytes, so every one of these is 6.2 KB/s. A hidden
// tab asks for none: `requestAnimationFrame` has stopped, so every frame sent
// to it would be received and thrown away. A tab on a connection that says it
// is slow, or whose owner has asked for less data, takes the lower rate - the
// picture is 64x32 and stays perfectly legible at ten frames a second.
//
// **The Panel screen asks for none at all** (card 198), for the same reason a
// hidden tab does: it draws no canvas, so every frame sent to it would be
// received and thrown away. It still gets the state and the heartbeat, which
// is what it is made of.
const PREVIEW_FPS = 30;
const PREVIEW_FPS_SLOW = 10;

export function previewFps() {
  if (document.hidden) return 0;
  const link = navigator.connection;
  if (link && (link.saveData || /^([23]g|slow-2g)$/.test(link.effectiveType || ''))) return PREVIEW_FPS_SLOW;
  return PREVIEW_FPS;
}

/** A screen that draws no pictures. */
export const noFrames = () => 0;

// One socket: binary messages are frames, text messages say what they are.
// It reconnects by itself, because the server is allowed to be restarted.
//
// The only thing a page ever sends on it is its pace (above). `repeat:false`
// says not to send a picture identical to the last one this socket got - a
// clock holding the time is the same 6196 bytes for fifteen seconds - which the
// server honours without letting the meters freeze.
export function connect(handlers, pace = previewFps) {
  // Root-absolute rather than relative to the document: `/panel` and `/panel/`
  // are the same page, and a relative URL would aim the socket at
  // `/panel/api/v1/ws` from the second of them.
  const url = new URL('/api/v1/ws', location.href);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  url.searchParams.set('client', CLIENT);
  url.searchParams.set('repeat', 'false');
  let wait = 250;
  let live = null;
  const ask = () => {
    // The rate goes in the query string too, so a page loaded in a background
    // tab never costs a frame - not even the one between opening and asking.
    url.searchParams.set('fps', String(pace()));
    if (live && live.readyState === WebSocket.OPEN) {
      live.send(JSON.stringify({ type: 'preview', fps: pace(), repeat: false }));
    }
  };
  const open = () => {
    ask();
    const socket = new WebSocket(url);
    socket.binaryType = 'arraybuffer';
    socket.addEventListener('open', () => { wait = 250; live = socket; notice(''); });
    socket.addEventListener('message', (e) => {
      if (e.data instanceof ArrayBuffer) { handlers.frame?.(e.data); return; }
      const message = JSON.parse(e.data);
      handlers[message.type]?.(message);
    });
    socket.addEventListener('close', () => {
      if (live === socket) live = null;
      notice('Lost contact with the studio. Reconnecting…');
      setTimeout(open, wait);
      wait = Math.min(wait * 2, 5000);
    });
    socket.addEventListener('error', () => socket.close());
  };
  document.addEventListener('visibilitychange', ask);
  navigator.connection?.addEventListener('change', ask);
  open();
}

/** The panel's own facts, polled: `GET /api/v1/status` every couple of seconds
 *  while the tab is visible, and once however the tab starts.
 *
 *  One read whatever the tab is doing, so a page opened in a background tab is
 *  already right the moment somebody looks at it. It is the *repeat* that a
 *  hidden tab is spared: a phone in a pocket should not poll all night.
 *
 *  Returns `read()`, for a screen that has just changed something and wants
 *  the answer now. */
export function pollStatus(got) {
  let timer = null;
  const read = async () => {
    let picture;
    try {
      picture = await invoke('status');
    } catch {
      return; // the socket's own reconnect notice covers this
    }
    got(picture);
  };
  const tick = () => {
    clearInterval(timer);
    if (document.hidden) return;
    read();
    timer = setInterval(read, STATUS_MS);
  };
  document.addEventListener('visibilitychange', tick);
  read();
  tick();
  return read;
}

// ---------- what the panel is doing ----------

/** The one judgement both screens draw: is it on the panel?
 *
 *  It is given the half-second heartbeat's link rather than the two-second
 *  poll, so a panel going away shows up in half a second and the answer does
 *  not depend on a read that may not have happened yet. */
export function panelState({ attached, device, on, link }) {
  const player = (device || {}).player;
  if (!attached) return { key: 'none', label: 'No panel', tone: 'away' };
  if (player && player.health.gave_up) return { key: 'stopped', label: 'Stopped', tone: 'bad' };
  if (!on) return { key: 'off', label: 'Output off', tone: 'away' };
  if (link && link.connected) return { key: 'live', label: 'On the panel', tone: 'on' };
  return { key: 'away', label: 'Panel away', tone: 'away' };
}

/** What the panel needs somebody to go and look at, in one phrase, worst
 *  first - and `''` when there is nothing, which is the ordinary case.
 *
 *  This is what stops trouble hiding behind a tab: the Picture screen's chip
 *  wears it, so a panel that has run out of stack is visible from the screen
 *  that shows none of the device's facts. Every one of these is a flag the
 *  **server** decided (`devices.rs`, beside the reasoning for its thresholds);
 *  a number here would be a second opinion. Only the fault-level ones are
 *  attention: a margin going (`stack_warn`) is said on the Panel screen in the
 *  warning tone and is not a reason to colour the other screen.
 *
 *  Card 199: `device.panic.repeat` - several panics in a row, each within a
 *  minute of a boot - is the one fact from `GET /api/v1/panic` that belongs
 *  here. An isolated panic, a watchdog reset or an update's outcome are said
 *  on the Panel screen but do not by themselves colour this chip: a panel
 *  that panicked once and came back is not what "go and look at it" means. */
export function attention(device) {
  if (!device) return '';
  const player = device.player;
  if (player && player.health.gave_up) return 'stopped';
  if (device.panic && device.panic.repeat) return 'repeated panics';
  const f = device.facts;
  if (f) {
    if (f.store_errors) return 'store errors';
    if (f.stack_fault) return 'out of stack';
    if (f.low_heap) return 'out of memory';
    if (f.odd_reset) return 'unexpected reset';
    if (f.bad_fw_state) return 'firmware slot';
  }
  if (device.last_error) return 'last error';
  return '';
}

// ---------- small control helpers ----------

/* Card 183: draw a range input's `list` on the track.
 *
 * Chrome paints tick marks for a `<datalist>` only on the *default* track, and
 * style.css replaces `::-webkit-slider-runnable-track`, so the stops were real
 * to the accessibility tree and invisible on the page - a declaration nobody
 * could see. They are drawn here instead, from the datalist itself, so there is
 * still one list of stops and adding a `list=` to any slider draws it.
 *
 * The geometry is the thumb's, not the track's: a 7px thumb inside a full-width
 * input puts its centre at `3.5px + frac * (W - 7px)`, so a mark at `frac%`
 * would be out by up to half a thumb - visibly wrong at the right-hand end.
 * `calc()` does the same sum the browser does, which also makes it correct at
 * every width without measuring anything.
 *
 * **They do not snap.** Card 172's worker decided that deliberately, card 183
 * kept it, and card 197 kept it again for Speed: a magnet at 1.00 makes 0.95
 * and 1.05 unreachable with a mouse, and a speed a script set must be shown
 * exactly rather than quietly rounded to the nearest stop. The marks say where
 * the useful values are; the arrow keys, the readout and - for Speed - a
 * double-click do the rest. */
export function drawStops(root) {
  const input = root.querySelector('input[type="range"][list]');
  if (!input) return;
  const list = document.getElementById(input.getAttribute('list'));
  if (!list) return;
  const min = Number(input.min), max = Number(input.max);
  if (!(max > min)) return;
  const strip = document.createElement('div');
  strip.className = 'stops';
  strip.ariaHidden = 'true';
  for (const option of list.options) {
    const at = (Number(option.value) - min) / (max - min);
    if (!(at >= 0 && at <= 1)) continue;
    const mark = document.createElement('i');
    // The same arithmetic the thumb does. 3.5px and 7px are the thumb's half
    // width and width in style.css; keep them in step.
    mark.style.left = `calc(3.5px + ${at} * (100% - 7px))`;
    if (option.label) mark.title = option.label;
    strip.append(mark);
  }
  if (strip.children.length) input.after(strip);
}

export function bindSlider(root, { get, set, format }) {
  const input = root.querySelector('input');
  const out = root.querySelector('output');
  const show = () => { out.textContent = format(Number(input.value)); };
  input.value = get();
  show();
  input.addEventListener('input', () => { set(Number(input.value)); show(); });
  return { refresh() { if (!busy(input)) { input.value = get(); } show(); }, show };
}

export function bindRadios(root, { get, set }) {
  const inputs = [...root.querySelectorAll('input')];
  const refresh = () => inputs.forEach((i) => { i.checked = i.value === String(get()); });
  inputs.forEach((i) => i.addEventListener('change', () => i.checked && set(i.value)));
  refresh();
  return { refresh };
}

export function bindSwitch(input, { get, set }) {
  const refresh = () => { input.checked = get(); };
  refresh();
  input.addEventListener('change', () => set(input.checked));
  return { refresh };
}

/** Brightness: the same control on both screens (card 198).
 *
 *  It is on the Picture screen because it changes how the picture looks on the
 *  LEDs and is part of judging a patch, and on the Panel screen because that is
 *  where the panel's controls are. One binding, so the two cannot drift: the
 *  maximum is the device's own ceiling once it has said what that is, the
 *  lowest non-zero stop is 6 (card 136), and what is shown is what the device
 *  says it applied.
 *
 *  `attached()` gives the id to act on, or `''`; `attempt` is the screen's. */
export function bindBrightness({ input, out, note, attached, attempt }) {
  input.addEventListener('input', () => {
    input.value = String(snapBrightness(Number(input.value)));
    out.textContent = input.value;
  });
  input.addEventListener('change', () => {
    const device = attached();
    if (!device) { notice('No panel is attached, so there is no brightness to set.', 'say'); return; }
    const level = snapBrightness(Number(input.value));
    attempt(`Brightness ${level}`, async () => {
      const done = await invoke('device/brightness', { device, level });
      return done && done.applied !== done.asked
        ? `This panel caps brightness at ${done.applied}.`
        : `Brightness ${done.applied}`;
    });
  });
  return {
    /** `d` is the attached device from the poll, or null: it can legitimately
     *  be a moment behind, so nothing here assumes it is there. */
    show(d) {
      const player = d && d.player;
      const cap = (player && player.health.brightness_cap) || 255;
      if (input.max !== String(cap)) input.max = String(cap);
      const t = d && d.telemetry;
      const shown = t ? t.brightness : player && (player.health.brightness_applied ?? player.brightness);
      if (!busy(input) && shown !== null && shown !== undefined) {
        input.value = String(Math.min(shown, cap));
        out.textContent = input.value;
      }
      note.textContent = !d
        ? 'No panel is attached, so this sets nothing yet.'
        : player && player.brightness !== null && player.brightness !== undefined
          ? `Kept at ${player.brightness} across reconnects.`
          : 'Not managed: whatever the panel has.';
    },
  };
}
