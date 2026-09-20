/* The dashboard.
 *
 * One read - GET /api/v1/status - every couple of seconds, and a handful of
 * POSTs. Deliberately *not* on the preview WebSocket: a dashboard left open on
 * a phone must not cost 370 KB/s of frames it does not draw (card 120).
 *
 * A card is built once per device and then updated in place, because rebuilding
 * it would fight a slider that is under a thumb. Anything focused is left
 * alone until it is not. No framework, no build step, no CDN: this runs on a
 * box that has no promise of internet.
 */

'use strict';

/** How often to re-read the whole picture. */
const POLL_MS = 2000;

/* Card 136: the firmware has 25 real brightness steps and values 1..=5 light
 * nothing at all while `applied` cheerfully echoes them back. So the slider's
 * lowest non-zero stop is the first value that lights the panel. Delete this
 * constant and the one snap() call below when 136 fixes the firmware. */
const BRIGHTNESS_FLOOR = 6;
const snapBrightness = (v) => (v > 0 ? Math.max(v, BRIGHTNESS_FLOOR) : 0);

const $ = (sel, root = document) => root.querySelector(sel);
const cardsEl = $('#cards');
const template = $('#card-template');

/** id -> the card element and the last status we drew into it. */
const cards = new Map();
let pieces = [];
let timer = null;

// ---------------------------------------------------------------- the wire --

async function api(path, body) {
  const opts = body === undefined
    ? {}
    : { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) };
  const res = await fetch('/api/v1/' + path, opts);
  const text = await res.text();
  let data = null;
  try { data = text ? JSON.parse(text) : null; } catch { data = null; }
  if (!res.ok) {
    throw new Error((data && data.error) || text || ('HTTP ' + res.status));
  }
  return data;
}

let noticeTimer = null;
function say(message, tone) {
  const el = $('#notice');
  el.textContent = message;
  el.hidden = !message;
  if (tone) { el.dataset.tone = tone; } else { delete el.dataset.tone; }
  clearTimeout(noticeTimer);
  if (message) { noticeTimer = setTimeout(() => { el.hidden = true; }, 6000); }
}

/** Run something that talks to a panel, and put whatever it says on the line. */
async function attempt(what, fn) {
  try {
    const out = await fn();
    say(what, null);
    return out;
  } catch (e) {
    say(what + ': ' + e.message, 'bad');
    return null;
  } finally {
    refresh();
  }
}

// ------------------------------------------------------------- formatting ---

const nf = new Intl.NumberFormat();

function ago(seconds) {
  if (seconds === null || seconds === undefined) return 'never';
  if (seconds < 2) return 'just now';
  if (seconds < 90) return Math.round(seconds) + ' s ago';
  if (seconds < 5400) return Math.round(seconds / 60) + ' min ago';
  return Math.round(seconds / 3600) + ' h ago';
}

function duration(seconds) {
  if (seconds === null || seconds === undefined) return '–';
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (d) return d + ' d ' + h + ' h';
  if (h) return h + ' h ' + m + ' min';
  if (m) return m + ' min';
  return Math.round(seconds) + ' s';
}

/** Fill a <dl> from [label, value, tone] triples, reusing its rows. */
function facts(dl, rows) {
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

/** Never fight an input somebody is using. */
const busy = (el) => el === document.activeElement;

function setValue(el, value) {
  if (busy(el)) return;
  const text = String(value);
  if (el.value !== text) el.value = text;
}

// ------------------------------------------------------------- the cards ----

function pieceOptions(select, current) {
  const want = pieces.map((p) => p.id);
  const have = Array.from(select.options).map((o) => o.value);
  if (want.join('\u0000') !== have.join('\u0000')) {
    select.textContent = '';
    for (const p of pieces) {
      const o = document.createElement('option');
      o.value = p.id;
      o.textContent = p.name;
      select.append(o);
    }
  }
  // A piece that is not on the menu (an old state file, a fault piece) still
  // has to be shown, or the select would silently claim something else.
  if (current && !want.includes(current)) {
    const o = document.createElement('option');
    o.value = current;
    o.textContent = current;
    select.append(o);
  }
  setValue(select, current || '');
}

function build(id) {
  const el = template.content.firstElementChild.cloneNode(true);
  const player = (change) => attempt('Changed ' + id, () => api('player/set', Object.assign({ device: id }, change)));

  $('.piece', el).addEventListener('change', (e) => player({ piece: e.target.value }));
  $('.fps', el).addEventListener('change', (e) => player({ fps: Number(e.target.value) }));
  $('.on', el).addEventListener('change', (e) => player({ on: e.target.checked }));
  $('.seed', el).addEventListener('change', (e) => player({ seed: Number(e.target.value) || 0 }));
  $('.new-seed', el).addEventListener('click', () => player({ seed: Math.floor(Math.random() * 1000000) }));

  const bright = $('.bright', el);
  bright.addEventListener('input', () => {
    bright.value = String(snapBrightness(Number(bright.value)));
    $('.bright-out', el).textContent = bright.value;
  });
  bright.addEventListener('change', () => {
    const level = snapBrightness(Number(bright.value));
    attempt('Brightness ' + level, async () => {
      const out = await api('device/brightness', { device: id, level });
      if (out && out.applied !== out.asked) {
        say('This panel caps brightness at ' + out.applied + '.', null);
      }
      return out;
    });
  });

  $('.adopt', el).addEventListener('click', () =>
    attempt('This panel now plays what the design view is showing', () => api('player/adopt_preview', { device: id })));
  $('.identify', el).addEventListener('click', () =>
    attempt('Identifying', () => api('device/identify', { device: id, ms: 3000 })));
  $('.rename', el).addEventListener('click', () => {
    const now = $('.card__name', el).textContent;
    const name = window.prompt('What should this panel be called?', now);
    if (name === null) return;
    attempt('Renamed', () => api('device/name', { device: id, name: name.trim() }));
  });
  $('.reboot', el).addEventListener('click', () => {
    const name = $('.card__name', el).textContent;
    if (!window.confirm('Reboot ' + name + '? It will go dark for a few seconds.')) return;
    attempt('Rebooting ' + name, () => api('device/reboot', { device: id, confirm: true }));
  });
  $('.forget', el).addEventListener('click', () => {
    const name = $('.card__name', el).textContent;
    if (!window.confirm('Forget ' + name + '? Its player and its settings go with it.')) return;
    attempt('Forgot ' + name, () => api('devices/forget', { device: id }));
  });

  cardsEl.append(el);
  return el;
}

/** The one thing you read from across the room. */
function pill(device) {
  const p = device.player;
  if (!p) return ['No player', 'away'];
  if (p.health.gave_up) return ['Stopped', 'bad'];
  if (!p.on) return ['Off', 'away'];
  if (!p.running) return ['Starting', 'away'];
  if (p.panel && p.panel.connected) return ['Playing', 'playing'];
  return ['Panel away', 'away'];
}

function draw(device) {
  const id = device.id;
  let el = cards.get(id);
  if (!el) { el = build(id); cards.set(id, el); }

  const p = device.player;
  const t = device.telemetry;
  const panel = p && p.panel;

  if ($('.card__name', el).textContent !== device.label) $('.card__name', el).textContent = device.label;
  const sub = device.frame_addr || device.address || device.instance || '';
  const idLine = sub ? device.id + ' · ' + sub : device.id;
  if ($('.card__id', el).textContent !== idLine) $('.card__id', el).textContent = idLine;

  const [label, state] = pill(device);
  const pillEl = $('.pill', el);
  if (pillEl.textContent !== label) pillEl.textContent = label;
  pillEl.dataset.state = state;

  pieceOptions($('.piece', el), p ? p.piece : '');
  setValue($('.seed', el), p ? p.seed : 0);
  setValue($('.fps', el), p ? p.fps : 30);
  const onEl = $('.on', el);
  if (!busy(onEl)) onEl.checked = !!(p && p.on);

  const fallback = $('.fallback', el);
  const trouble = p && (p.health.gave_up || (p.health.fell_back_from ? 'Fell back from `' + p.health.fell_back_from + '`.' : ''));
  fallback.textContent = trouble || '';
  fallback.hidden = !trouble;

  // Brightness. The maximum is the device's own ceiling once it has told us
  // what that is, so the slider cannot ask for something it will not give.
  const bright = $('.bright', el);
  const cap = (p && p.health.brightness_cap) || 255;
  if (bright.max !== String(cap)) bright.max = String(cap);
  const level = p && (p.health.brightness_applied ?? p.brightness);
  const shown = t ? t.brightness : level;
  if (!busy(bright) && shown !== null && shown !== undefined) {
    bright.value = String(Math.min(shown, cap));
    $('.bright-out', el).textContent = bright.value;
  }
  $('.bright-note', el).textContent = p && p.brightness !== null && p.brightness !== undefined
    ? 'Kept at ' + p.brightness + ' across reconnects.'
    : 'Not managed: whatever the panel has.';

  const rows = [];
  if (panel) {
    rows.push(['Link', panel.state + (panel.connected ? ' · ' + panel.fps.toFixed(0) + ' fps' : ''), panel.connected ? null : 'dim']);
    rows.push(['Frames', nf.format(panel.frames_sent) + ' sent, ' + nf.format(panel.frames_coalesced) + ' folded, ' + nf.format(panel.frames_dropped) + ' lost']);
    if (panel.codec_name) {
      rows.push(['Last frame', panel.codec_name + ', ' + panel.bytes + ' B' + (panel.exact ? ', exact' : ', requantised')]);
    }
    if (panel.indexed_fallback) rows.push(['Requantised', nf.format(panel.indexed_fallback), 'warn']);
  }
  if (p) {
    rows.push(['Reconnects', Math.max(0, p.health.sessions - 1)]);
    rows.push(['Rendered', nf.format(p.health.ticks) + ' frames at ' + p.fps_measured.toFixed(0) + ' fps']);
    if (p.health.panics || p.health.stalls) {
      rows.push(['Faults', p.health.panics + ' panics, ' + p.health.stalls + ' stalls, ' + p.health.restarts + ' restarts', 'warn']);
    }
  }
  rows.push(['Heard', ago(device.last_seen_ago), device.last_seen_ago > 60 ? 'dim' : null]);
  if (t) {
    rows.push(['Panel', t.state + ' · ' + nf.format(t.frames_shown) + ' shown']);
    rows.push(['Up', duration(t.uptime_s)]);
    rows.push(['Signal', t.rssi_dbm + ' dBm', t.rssi_dbm < -75 ? 'warn' : null]);
    const drops = t.drops.stale + t.drops.superseded + t.drops.decode + t.drops.rejected;
    rows.push([
      'Dropped',
      drops === 0 && t.drops.seq_gaps === 0
        ? 'none'
        : t.drops.stale + ' stale, ' + t.drops.superseded + ' superseded, ' + t.drops.decode + ' decode, ' + t.drops.rejected + ' rejected, ' + t.drops.seq_gaps + ' gaps',
      drops ? 'warn' : null,
    ]);
  }
  if (device.firmware) rows.push(['Firmware', device.firmware + (device.panel_size ? ' · ' + device.panel_size : '')]);
  if (device.last_error) rows.push(['Last error', device.last_error, 'bad']);
  facts($('.facts', el), rows);
}

// -------------------------------------------------------------- the page ----

function drawServer(status) {
  const ok = status.ok;
  $('#server-line').textContent =
    (ok ? 'Well' : 'NOT WELL') + ' · up ' + duration(status.uptime_s) + ' · v' + status.version;
  facts($('#server-facts'), [
    ['Health', ok ? 'ok' : 'unhealthy', ok ? null : 'bad'],
    ['State', status.state.persisting ? status.state.path : 'in memory only (nothing is saved)', status.state.persisting ? null : 'warn'],
    ['Saved', status.state.last_error ? status.state.last_error : nf.format(status.state.writes) + ' times', status.state.last_error ? 'bad' : null],
    ['Discovery', status.discovery.enabled
      ? nf.format(status.discovery.browses) + ' browses, ' + status.discovery.last_found + ' found'
      : 'off · configured addresses only', status.discovery.enabled ? null : 'dim'],
    ['Design view', (status.preview.piece || '–') + (status.preview.wedged ? ' · WEDGED' : '') +
      (status.preview.panel_on ? ' · sending to ' + status.preview.panel_to : ''), status.preview.wedged ? 'bad' : null],
  ]);
  const problems = $('#server-problems');
  problems.textContent = status.problems.join(' · ');
  problems.dataset.tone = status.problems.length ? 'bad' : '';
}

async function refresh() {
  let status;
  try {
    status = await api('status');
  } catch (e) {
    $('#server-line').textContent = 'The server is not answering: ' + e.message;
    return;
  }

  const seen = new Set();
  for (const device of status.devices) {
    seen.add(device.id);
    draw(device);
  }
  for (const [id, el] of cards) {
    if (!seen.has(id)) { el.remove(); cards.delete(id); }
  }
  $('#empty').hidden = status.devices.length > 0;
  drawServer(status);
}

async function start() {
  try {
    const boot = await api('bootstrap');
    pieces = boot.pieces;
  } catch {
    pieces = [];
  }

  $('#add').addEventListener('click', async () => {
    const to = $('#add-to').value.trim();
    if (!to) { say('Type a name or an address first.', 'bad'); return; }
    const name = $('#add-name').value.trim();
    const done = await attempt('Added ' + to, () => api('devices/add', { to, name, play: true }));
    if (done) { $('#add-to').value = ''; $('#add-name').value = ''; }
  });
  $('#refresh').addEventListener('click', () =>
    attempt('Asked every panel who it is', () => api('devices/refresh', {})));

  await refresh();
  // A hidden tab costs nothing: a phone in a pocket should not poll all night.
  document.addEventListener('visibilitychange', tick);
  tick();
}

function tick() {
  clearInterval(timer);
  if (document.hidden) return;
  refresh();
  timer = setInterval(refresh, POLL_MS);
}

start();
