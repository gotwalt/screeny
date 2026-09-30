// What all of the Studio's screens are made of.
//
// There are three screens - the **Picture** (`/`), the **Panel** (`/panel`)
// and the **Settings** (`/settings`, card 311, in the place card 303's
// Schedule had until card 310 retired it) - and one server behind them. What
// they share is everything that is not layout: the socket, the poll, the
// formatting, the small control bindings, and the one judgement of what the
// panel is doing. Each screen's own file (`picture.js`, `panel.js`,
// `settings.js`) imports from here and touches only the elements its own page
// has.
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

// Reads; everything else is a POST carrying its arguments as JSON. A read's
// arguments go in the query string instead - `?panel=` (card 351) is the one
// any of them takes.
const GETS = new Set(['bootstrap', 'frame', 'patch_playing', 'panel_status', 'status', 'devices', 'home_assistant', 'panels']);

export async function invoke(cmd, args) {
  const read = GETS.has(cmd);
  const init = read ? {} : {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-studio-client': CLIENT },
    body: JSON.stringify(args ?? {}),
  };
  const url = new URL(`/api/v1/${cmd}`, location.href);
  if (read && args) {
    for (const [k, v] of Object.entries(args)) {
      if (v !== undefined && v !== null && v !== '') url.searchParams.set(k, String(v));
    }
  }
  const response = await fetch(url, init);
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
// **The Panel and Settings screens ask for none at all** (card 198, 301,
// 311), for the same reason a hidden tab does: neither draws a canvas, so
// every frame sent to either would be received and thrown away. They still get the state
// and the heartbeat, which is what they are made of.
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
//
// Card 351: a socket is about one panel. `panel` is the device id it names
// (`''` follows the first panel, as a socket always did); `overview: false`
// asks it to leave out the `{"type":"panels"}` messages, which only a screen's
// own socket needs; `quiet` keeps a thumbnail's socket off the notice line,
// which the screen's own socket already speaks on. The answer is a handle
// whose `close()` stops it for good - a thumbnail whose panel is forgotten, or
// has gone idle, is closed rather than left reconnecting.
export function connect(handlers, pace = previewFps, { panel = '', overview = true, quiet = false } = {}) {
  // Root-absolute rather than relative to the document: `/panel` and `/panel/`
  // are the same page, and a relative URL would aim the socket at
  // `/panel/api/v1/ws` from the second of them.
  const url = new URL('/api/v1/ws', location.href);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  url.searchParams.set('client', CLIENT);
  url.searchParams.set('repeat', 'false');
  if (panel) url.searchParams.set('panel', panel);
  if (!overview) url.searchParams.set('overview', 'false');
  let wait = 250;
  let live = null;
  let closed = false;
  let timer = null;
  const ask = () => {
    // The rate goes in the query string too, so a page loaded in a background
    // tab never costs a frame - not even the one between opening and asking.
    url.searchParams.set('fps', String(pace()));
    if (live && live.readyState === WebSocket.OPEN) {
      live.send(JSON.stringify({ type: 'preview', fps: pace(), repeat: false }));
    }
  };
  const open = () => {
    if (closed) return;
    ask();
    const socket = new WebSocket(url);
    socket.binaryType = 'arraybuffer';
    socket.addEventListener('open', () => { wait = 250; live = socket; if (!quiet) notice(''); });
    socket.addEventListener('message', (e) => {
      if (e.data instanceof ArrayBuffer) { handlers.frame?.(e.data); return; }
      const message = JSON.parse(e.data);
      handlers[message.type]?.(message);
    });
    socket.addEventListener('close', () => {
      if (live === socket) live = null;
      if (closed) return;
      if (!quiet) notice('Lost contact with the studio. Reconnecting…');
      timer = setTimeout(open, wait);
      wait = Math.min(wait * 2, 5000);
    });
    socket.addEventListener('error', () => socket.close());
  };
  document.addEventListener('visibilitychange', ask);
  navigator.connection?.addEventListener('change', ask);
  open();
  return {
    close() {
      closed = true;
      clearTimeout(timer);
      document.removeEventListener('visibilitychange', ask);
      navigator.connection?.removeEventListener('change', ask);
      live?.close();
    },
  };
}

// ---------- which panel (card 351) ----------
//
// A studio drives several panels (card 350), and which one a screen is about
// is **the page's** business: `?panel=<device id>` in the URL, so a reload or
// a bookmark keeps it and the back button is the browser's job. No `?panel=`
// means the first panel, which is what every route and socket means without
// one - so a studio with one panel reads exactly as it did.

/** The panel this page was opened on: the device id in `?panel=`, or `''`. */
export const chosenPanel = () => new URLSearchParams(location.search).get('panel') || '';

/** `path` about panel `id` - no query at all for `''`, the first panel. */
export function panelHref(path, id) {
  return id ? `${path}?panel=${encodeURIComponent(id)}` : path;
}

/** Carry the panel across the nav and the status chip, so the Panel screen
 *  opens on the panel the Picture screen was showing, and back again. Only
 *  once a panel was chosen: without one every screen means the first. */
export function carryPanel(id) {
  if (!chosenPanel() || !id) return;
  document.querySelectorAll('.nav a, a.pill--link').forEach((a) => {
    a.href = panelHref(new URL(a.href, location.href).pathname, id);
  });
}

/** A `?panel=` naming a panel the studio has not got - forgotten since the
 *  bookmark was made - lands on the first panel rather than on an error. */
export function forgetChoice() {
  if (chosenPanel()) location.replace(location.pathname);
}

/** Bootstrap for the chosen panel. A chosen panel the studio has not got is
 *  the server's 404, and the page goes to the first panel instead. */
export async function bootstrapFor(panel) {
  try {
    return await invoke('bootstrap', { panel });
  } catch (e) {
    if (panel) forgetChoice();
    throw e;
  }
}

/** A frame packet: this header, then 64x32 sRGB (keep in step with
 *  studio/src/page.rs). */
export const HEADER = 52;
export const W = 64;
export const H = 32;

/** Card 351: the overview's link word (`PanelSummary.link`) as a person reads
 *  it, and the tone of the dot beside it. */
const LINK_WORDS = {
  up: ['Live', 'on'],
  connecting: ['Connecting', 'away'],
  waiting: ['Waiting', 'away'],
  closed: ['Away', 'away'],
  off: ['Output off', 'off'],
  idle: ['Idle', 'idle'],
  none: ['No panel', 'off'],
};
export const linkWords = (link) => LINK_WORDS[link] || [words(link || ''), 'away'];

/** One line for what a panel is showing: "Metaballs · Lava", and
 *  "modified" when its channel has been moved since. */
export function pictureLine(p) {
  if (!p.picture) return 'Idle: no picture yet';
  const pic = p.picture;
  return [pic.patch_name || pic.patch, pic.setting, pic.modified ? 'modified' : ''].filter(Boolean).join(' · ');
}

/** **The panel row** (card 351): one card per panel - a live thumbnail of
 *  what it is showing, its name, its picture and a dot for its link - each a
 *  link to `path?panel=<id>`. The Picture screen gives it thumbnails; the
 *  Panel screen, which draws no pictures (card 120, 198), does not.
 *
 *  Thumbnails are cheap on purpose: every panel but the one the screen is
 *  about gets its own socket at four frames a second (`?fps=4&repeat=false
 *  &overview=false`, the README's recipe), and none while the tab is hidden -
 *  `pace` is the screen's own, so a hidden tab asks 0 here too. The one the
 *  screen is about is drawn from frames the screen already has (`frame()`),
 *  so it is the only panel at full rate and costs no second socket. An idle
 *  panel has no picture, and so no socket either.
 *
 *  `root` is the element to fill; `current()` the device id the screen is
 *  about. `update(panels)` takes the overview (`{"type":"panels"}`). */
export function panelRow(root, { path, current, thumbs = false, pace = previewFps }) {
  /** device id -> the card's elements and its socket */
  const cards = new Map();
  const THUMB_FPS = 4;
  const thumbPace = () => Math.min(THUMB_FPS, pace());

  const paint = (card, buf) => {
    if (!card.ctx || buf.byteLength < HEADER + W * H * 3) return;
    const rgb = new Uint8Array(buf, HEADER, W * H * 3);
    const px = card.image.data;
    for (let i = 0, j = 0; i < W * H * 3; i += 3, j += 4) {
      px[j] = rgb[i]; px[j + 1] = rgb[i + 1]; px[j + 2] = rgb[i + 2]; px[j + 3] = 255;
    }
    card.ctx.putImageData(card.image, 0, 0);
  };

  const span = (className) => Object.assign(document.createElement('span'), { className });

  const make = (id) => {
    const el = document.createElement('a');
    el.className = 'pcard';
    el.href = panelHref(path, id);
    const card = { el, socket: null };
    if (thumbs) {
      const face = span('pcard__face');
      const canvas = Object.assign(document.createElement('canvas'), { className: 'pcard__thumb', width: W, height: H });
      face.append(canvas);
      el.append(face);
      card.ctx = canvas.getContext('2d');
      card.image = card.ctx.createImageData(W, H);
    }
    card.name = span('pcard__name');
    card.pic = span('pcard__pic');
    card.state = span('pcard__state');
    el.append(card.name, card.pic, card.state);
    return card;
  };

  const text = (el, t) => { if (el.textContent !== t) el.textContent = t; };

  function update(panels) {
    const here = current();
    const byId = new Map(panels.map((p) => [p.device, p]));
    const name = (id) => { const p = byId.get(id); return (p && p.name) || id || 'Panel'; };
    for (const [id, card] of cards) {
      if (!byId.has(id)) { card.socket?.close(); card.el.remove(); cards.delete(id); }
    }
    panels.forEach((p, i) => {
      let card = cards.get(p.device);
      if (!card) { card = make(p.device); cards.set(p.device, card); }
      if (root.children[i] !== card.el) root.insertBefore(card.el, root.children[i] || null);
      const mine = p.device === here;
      if (mine) { card.el.setAttribute('aria-current', 'page'); } else { card.el.removeAttribute('aria-current'); }
      card.el.dataset.idle = p.picture ? 'no' : 'yes';
      text(card.name, name(p.device));
      text(card.pic, pictureLine(p));
      const [said, tone] = linkWords(p.link);
      const shared = (p.shared_with || []).map(name);
      text(card.state, shared.length ? `${said} · with ${shared.join(', ')}` : said);
      card.state.dataset.state = tone;
      card.el.title = `${name(p.device)}: ${pictureLine(p)}`;
      // Its own socket for every other panel that has a picture to show.
      const wants = thumbs && !mine && Boolean(p.picture);
      if (wants && !card.socket) {
        card.socket = connect({ frame: (buf) => paint(card, buf) }, thumbPace, { panel: p.device, overview: false, quiet: true });
      } else if (!wants && card.socket) {
        card.socket.close();
        card.socket = null;
      }
      if (thumbs && !p.picture) card.ctx.clearRect(0, 0, W, H);
    });
    root.dataset.count = String(panels.length);
  }

  return {
    update,
    /** A frame of the panel this screen is about, from the screen's own socket. */
    frame(buf) {
      const card = cards.get(current());
      if (card) paint(card, buf);
    },
  };
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
export function panelState({ attached, device, on, link, idle = false }) {
  const player = (device || {}).player;
  if (!attached) return { key: 'none', label: 'No panel', tone: 'away' };
  if (player && player.health.gave_up) return { key: 'stopped', label: 'Stopped', tone: 'bad' };
  // Card 350/351: a panel with no picture - no channel, no stream - is idle,
  // on its own status screen, and that is not "away".
  if (idle) return { key: 'idle', label: 'Idle', tone: 'away' };
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

/** The status chip in the title block (card 198/303):
 *  the panel's name, what it is doing, and - where a rate means something -
 *  how fast it is being sent. One function rather than two, since card 307:
 *  the Picture and (card 303's) Schedule screens used to keep their own
 *  copies, and the Schedule one had drifted to leave "what it is doing" out
 *  entirely, so its chip read a bare device name with nothing beside it
 *  while the Picture screen's said "live · 30 fps".
 *
 *  Takes the chip element itself, per this file's own rule of reaching for
 *  nothing else by id - `#ro-panel` is not on the Panel screen, which has no
 *  need of a chip pointing at itself.
 *
 *  `rate` is the fps to show while live, or `null`/`undefined` where there is
 *  none worth showing - a screen with no canvas has no rate (card 198,
 *  301); the Picture screen passes the heartbeat's `link.fps`. */
export function showChip(chip, { attachedId, device, on, link, rate, idle = false }) {
  const here = panelState({ attached: Boolean(attachedId), device, on, link, idle });
  const name = device ? device.label : attachedId;
  const doing = here.key === 'live'
    ? (rate === null || rate === undefined ? 'live' : `live · ${rate.toFixed(0)} fps`)
    : here.key === 'off' ? 'output off'
      : here.key === 'away' ? 'away'
        : here.key === 'stopped' ? 'stopped'
          : here.key === 'idle' ? 'idle' : '';
  const needs = attention(device);
  const label = here.key === 'none' ? 'No panel' : [name || 'Panel', doing, needs].filter(Boolean).join(' · ');
  if (chip.textContent !== label) chip.textContent = label;
  chip.dataset.state = needs ? 'bad' : here.tone;
  return here;
}

/** Card 351: the chip on a screen that is about the whole studio rather than
 *  one panel (Settings). With one panel it is that panel's chip, as it always
 *  was; with several it says how many and how many are live, and takes the
 *  fault tone - and points at - the first panel that needs attention, so
 *  trouble on any panel is still never hidden behind this tab. `panels` is
 *  the overview, `devices` the status poll's list. */
export function showStudioChip(chip, { panels, devices }) {
  const real = (panels || []).filter((p) => !p.unbound);
  const deviceOf = (id) => (devices || []).find((d) => d.id === id) || null;
  if (real.length <= 1) {
    const p = real[0];
    const device = p ? deviceOf(p.device) : null;
    const link = p && p.connected ? { connected: true } : null;
    showChip(chip, { attachedId: p ? p.device : '', device, on: p ? p.on : false, link, rate: null, idle: Boolean(p && !p.picture) });
    chip.href = panelHref('/panel', chosenPanel());
    return;
  }
  const trouble = real.find((p) => attention(deviceOf(p.device)));
  const live = real.filter((p) => p.connected).length;
  const label = [`${real.length} panels`, live ? `${live} live` : 'none live', trouble ? `${trouble.name}: ${attention(deviceOf(trouble.device))}` : '']
    .filter(Boolean).join(' · ');
  if (chip.textContent !== label) chip.textContent = label;
  chip.dataset.state = trouble ? 'bad' : live ? 'on' : 'away';
  chip.href = panelHref('/panel', trouble ? trouble.device : chosenPanel());
}

// ---------- small control helpers ----------

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

/** Card 187: the stops a device's own cap actually allows - the server's
 *  static list (`boot.brightness_stops`, `page::brightness_stops` on the
 *  server, one implementation of the output-enable slot arithmetic) trimmed
 *  to `cap`, with `cap` itself appended if it is not already one of them. A
 *  cap is a raw byte the device reported once (`clamp_brightness` can hand
 *  one straight back whatever it is), so it does not always land exactly on
 *  a stop; appending it is what makes the top of the slider the device's
 *  real ceiling rather than the stop just under it. */
function cappedStops(stops, cap) {
  const kept = stops.filter((v) => v <= cap);
  if (kept.length === 0 || kept[kept.length - 1] !== cap) kept.push(cap);
  return kept;
}

/** Card 126: how long the control holds what the person just chose (or what
 *  the panel said it applied) against a reading that has not caught up yet.
 *
 *  The panel's telemetry is polled periodically (five seconds by default),
 *  so for a while after a `change` the freshest telemetry the page has still
 *  carries the OLD brightness - measured against `screeny-sim`, about 1.6 s
 *  of a ~5 s poll window. `player.health.brightness_applied` is already the
 *  true new number the moment the POST that set it resolves (`api.rs` writes
 *  it synchronously, so every browser's next state message carries it), so
 *  the bug was never a missing fact - it was `show()` always preferring
 *  telemetry over it whenever telemetry existed at all, stale or not. Five
 *  seconds comfortably brackets one poll interval; a change from elsewhere
 *  that is real (a second browser, the CLI, the device's own drift) still
 *  reaches the slider once telemetry disagrees for the whole of it. */
const BRIGHTNESS_HOLD_MS = 5000;

/** Whether a brightness hold started at `held` should still override
 *  `reading`, the value `show()` would otherwise paint. Pure so it can be
 *  pinned without a DOM: `now` and `until` are `Date.now()` milliseconds,
 *  `held` and `reading` are raw brightness bytes (`reading` may be `null` or
 *  `undefined` when there is nothing to compare against yet).
 *
 *  Releases the moment `reading` agrees with what was held - there is no
 *  reason to keep waiting out the clock once the panel has confirmed it -
 *  and otherwise once `until` passes, which is what lets a change made
 *  elsewhere eventually win instead of being held forever. */
export function brightnessHoldWins(now, until, held, reading) {
  if (reading !== null && reading !== undefined && reading === held) return false;
  return now < until;
}

/** Card 312: a brightness level as the share of full light it gives, in
 *  percent - the unit Home Assistant's slider uses too (card 310), so the two
 *  always say the same number. `stops` is `boot.brightness_stops`: `stops[k]`
 *  is the first level that lights `k` of the panel's output-enable slots, so
 *  the slots a level lights are the highest `k` with `stops[k] <= level`, and
 *  every slot is `100 / (stops.length - 1)` = 4 %. Exported for its test. */
export function percentOf(level, stops) {
  let slots = 0;
  for (let k = 0; k < stops.length; k += 1) {
    if (stops[k] <= level) slots = k; else break;
  }
  return Math.round((100 * slots) / (stops.length - 1));
}

/** Brightness: the same control on both screens (card 198), stepping
 *  through the panel's real resolution rather than `0..255` (card 187) - most
 *  of that range lands on a picture a neighbouring value already showed
 *  (`oe_slots(129) == oe_slots(130)`), so the slider is an **index** into
 *  `stops`, not the level itself. `0` is off, the lowest nonzero stop is
 *  the floor the firmware raises a too-dim request to, and the top stop is
 *  the device's own learned cap once there is one - all three come from the
 *  server, never written down again here.
 *
 *  Card 301 took the Picture screen's copy of this control off the page - it
 *  is a panel setting, not something that changes how a patch is judged - so
 *  the Panel screen is the one caller now. This stays a shared function rather
 *  than moving into `panel.js` outright, because "how brightness is stepped
 *  and held" is exactly the kind of thing that must not drift if a second
 *  caller ever comes back.
 *
 *  `attached()` gives the id to act on, or `''`; `attempt` is the screen's;
 *  `stops` is `boot.brightness_stops`, the full list before any cap. */
export function bindBrightness({ input, out, note, attached, attempt, stops }) {
  /** The stops actually reachable right now - `stops` until a cap is learned. */
  let live = stops;
  /** What a person reads: percent of full light (card 312), never the raw
   *  0-255 level, which only the wire needs. */
  const say = (level) => `${percentOf(level, stops)}%`;
  const levelAt = (i) => live[Math.min(Math.max(Math.round(i), 0), live.length - 1)];
  /** The index of the highest stop at or below `level` - so a level this
   *  slider is handed always maps to something the panel could really be
   *  showing, never one it would silently round up past. */
  const indexOf = (level) => {
    let idx = 0;
    for (let i = 0; i < live.length; i += 1) {
      if (live[i] <= level) idx = i; else break;
    }
    return idx;
  };

  input.step = '1';
  input.min = '0';
  input.max = String(live.length - 1);

  /** The one thing `show()` holds against a stale reading - see
   *  `brightnessHoldWins` - `null` when nothing is being held. */
  let held = null;

  /** Card 126, second half: `busy(input)` is "is this the focused element",
   *  and Safari - desktop and iOS both - never focuses an
   *  `<input type="range">` on a click or a touch, only on Tab. So on Safari
   *  `busy(input)` is false for the whole of a drag, and `show()` would
   *  rewrite `input.value` out from under the person's mouse or finger every
   *  time a fresh reading arrived - up to several times a second - which
   *  reads exactly as "I move the slider and it bounces around". `holding` is
   *  the same idea `busy` is reaching for, tracked directly instead of
   *  through focus: set the moment a gesture starts (`pointerdown`,
   *  `touchstart` - a touch that never becomes a pointer event on an older
   *  browser, `keydown`, and `input` itself, a backstop for whatever starts
   *  a change no earlier event caught), cleared once `change` says the
   *  gesture landed. `pointerup`/`pointercancel`/`blur` clear it too, as a
   *  backstop for a gesture that ends without ever firing `change` (a touch
   *  cancelled by a system gesture, a pointer that leaves the window). */
  let holding = false;
  const grab = () => { holding = true; };
  const release = () => { holding = false; };
  input.addEventListener('pointerdown', grab);
  input.addEventListener('touchstart', grab, { passive: true });
  input.addEventListener('keydown', grab);
  input.addEventListener('pointerup', release);
  input.addEventListener('pointercancel', release);
  input.addEventListener('blur', release);

  input.addEventListener('input', () => { grab(); out.textContent = say(levelAt(input.value)); });
  input.addEventListener('change', () => {
    release();
    const device = attached();
    if (!device) { notice('No panel is attached, so there is no brightness to set.', 'say'); return; }
    const level = levelAt(input.value);
    // Hold what was asked from the moment the gesture lands, not from
    // whenever the POST happens to resolve - the gap between them is exactly
    // the window in which nothing used to be held at all (`held` was still
    // `null`, or `busy` had already gone false on Safari), and `show()` could
    // paint a reading from before the change.
    held = { value: level, until: Date.now() + BRIGHTNESS_HOLD_MS };
    attempt(`Brightness ${say(level)}`, async () => {
      try {
        const done = await invoke('device/brightness', { device, level });
        if (!done) { held = null; return ''; }
        // Replace the held value with the true applied one, not what was
        // asked - the floor and the cap are real, and this is what stops the
        // slider bouncing back to them once telemetry catches up.
        held = { value: done.applied, until: Date.now() + BRIGHTNESS_HOLD_MS };
        if (done.applied > done.asked) return `Raised to ${say(done.applied)}, the dimmest this panel can show.`;
        if (done.applied < done.asked) return `This panel caps brightness at ${say(done.applied)}.`;
        return `Brightness ${say(done.applied)}`;
      } catch (e) {
        held = null; // the ask never landed; nothing to hold
        throw e;
      }
    });
  });
  return {
    /** `d` is the attached device from the poll, or null: it can legitimately
     *  be a moment behind, so nothing here assumes it is there. */
    show(d) {
      const player = d && d.player;
      const learnedCap = player && player.health.brightness_cap;
      live = learnedCap ? cappedStops(stops, learnedCap) : stops;
      const maxIndex = String(live.length - 1);
      if (input.max !== maxIndex) input.max = maxIndex;
      const t = d && d.telemetry;
      const reading = t ? t.brightness : player && (player.health.brightness_applied ?? player.brightness);
      if (!d) held = null; // no panel to hold anything against
      if (held && !brightnessHoldWins(Date.now(), held.until, held.value, reading)) held = null;
      const shown = held ? held.value : reading;
      if (!busy(input) && !holding && shown !== null && shown !== undefined) {
        input.value = String(indexOf(Math.min(shown, learnedCap || 255)));
        out.textContent = say(levelAt(input.value));
      }
      note.textContent = !d
        ? 'No panel is attached, so this sets nothing yet.'
        : player && player.brightness !== null && player.brightness !== undefined
          ? `Kept at ${say(player.brightness)} across reconnects.`
          : 'Not managed: whatever the panel has.';
    },
  };
}
