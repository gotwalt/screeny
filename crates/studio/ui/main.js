// Screeny Studio front end.
//
// One panel, one picture. The server renders on the panel's own player and the
// page is a window onto it: the frames drawn here are the same decoded
// datagrams the panel is being sent, and every control changes the panel.
//
// Frames, other browsers' changes and the half-second heartbeat arrive on one
// WebSocket; the panel's own facts - link, telemetry, the device list - come
// from GET /api/v1/status every couple of seconds while the tab is visible.
// Everything else is a POST to /api/v1/<command>.

'use strict';

const W = 64, H = 32;
const HEADER = 52; // keep in step with studio/src/page.rs
const PITCH_MM = 3; // LED pitch: the lit area is 192 x 96 mm
/** How often to re-read the panel's own facts. */
const STATUS_MS = 2000;

/* Card 136: the firmware has 25 real brightness steps and values 1..=5 light
 * nothing at all while `applied` cheerfully echoes them back. So the slider's
 * lowest non-zero stop is the first value that lights the panel. Delete this
 * constant and the one snap() call below when 136 fixes the firmware. */
const BRIGHTNESS_FLOOR = 6;
const snapBrightness = (v) => (v > 0 ? Math.max(v, BRIGHTNESS_FLOOR) : 0);

const $ = (sel, root = document) => root.querySelector(sel);

// ---------- per-viewer view settings (never sent to the server) ----------

const view = Object.assign(
  { mode: 'dots', size: 'fit', dot: 0.66, bloom: 0.3, pxPerMm: 4.96 }, // 4.96 = a 14" MacBook Pro
  readStored(),
);

function readStored() {
  try { return JSON.parse(localStorage.getItem('view') || '{}'); } catch { return {}; }
}

function storeView() {
  try { localStorage.setItem('view', JSON.stringify(view)); } catch { /* private window */ }
}

// ---------- LED renderer ----------

const VERT = `#version 300 es
in vec2 pos;
out vec2 uv;
void main() {
  uv = vec2(pos.x * 0.5 + 0.5, 0.5 - pos.y * 0.5);
  gl_Position = vec4(pos, 0.0, 1.0);
}`;

// All light is summed in linear space and encoded once at the end.
const FRAG = `#version 300 es
precision highp float;
uniform sampler2D frame;
uniform int mode;      // 0 LEDs, 1 squint, 2 pixels
uniform float dotSize; // LED diameter / pitch
uniform float bloom;
uniform float aa;      // one output pixel, in pitch units
in vec2 uv;
out vec4 colour;

vec3 toLinear(vec3 c) {
  return mix(c / 12.92, pow((c + 0.055) / 1.055, vec3(2.4)), step(0.04045, c));
}
vec3 toSrgb(vec3 c) {
  c = clamp(c, 0.0, 1.0);
  return mix(c * 12.92, 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, step(0.0031308, c));
}
vec3 led(ivec2 q) {
  if (q.x < 0 || q.y < 0 || q.x >= ${W} || q.y >= ${H}) return vec3(0.0);
  return toLinear(texelFetch(frame, q, 0).rgb);
}

void main() {
  vec2 p = uv * vec2(${W}.0, ${H}.0);
  ivec2 cell = ivec2(floor(p));
  vec3 c = vec3(0.0);
  if (mode == 2) {
    c = led(cell);
  } else if (mode == 1) {
    // Viewing distance: every LED becomes a Gaussian splat wide enough to
    // merge with its neighbours.
    float total = 0.0;
    for (int dy = -3; dy <= 3; dy++) for (int dx = -3; dx <= 3; dx++) {
      ivec2 q = cell + ivec2(dx, dy);
      vec2 d = p - (vec2(q) + 0.5);
      float w = exp(-dot(d, d) / (2.0 * 0.72 * 0.72));
      total += w;
      c += led(q) * w;
    }
    c /= total;
  } else {
    float r = dotSize * 0.5;
    for (int dy = -2; dy <= 2; dy++) for (int dx = -2; dx <= 2; dx++) {
      ivec2 q = cell + ivec2(dx, dy);
      float d = length(p - (vec2(q) + 0.5));
      float body = (1.0 - smoothstep(r - aa, r + aa, d)) * (1.0 - 0.22 * (d * d) / (r * r));
      float halo = exp(-d * d / (2.0 * 0.5 * 0.5)) * bloom * 0.12;
      c += led(q) * (body + halo);
    }
  }
  colour = vec4(toSrgb(c), 1.0);
}`;

function createRenderer(canvas) {
  const gl = canvas.getContext('webgl2', { antialias: false, alpha: false });
  if (!gl) throw new Error('This browser has no WebGL 2, so the panel cannot be drawn.');

  const compile = (type, src) => {
    const sh = gl.createShader(type);
    gl.shaderSource(sh, src);
    gl.compileShader(sh);
    if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(sh));
    return sh;
  };
  const prog = gl.createProgram();
  gl.attachShader(prog, compile(gl.VERTEX_SHADER, VERT));
  gl.attachShader(prog, compile(gl.FRAGMENT_SHADER, FRAG));
  gl.linkProgram(prog);
  if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(prog));
  gl.useProgram(prog);

  gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
  const loc = gl.getAttribLocation(prog, 'pos');
  gl.enableVertexAttribArray(loc);
  gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);

  gl.bindTexture(gl.TEXTURE_2D, gl.createTexture());
  gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGB8, W, H, 0, gl.RGB, gl.UNSIGNED_BYTE, new Uint8Array(W * H * 3));

  const u = Object.fromEntries(['mode', 'dotSize', 'bloom', 'aa'].map((n) => [n, gl.getUniformLocation(prog, n)]));

  return {
    upload(rgb) {
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, W, H, gl.RGB, gl.UNSIGNED_BYTE, rgb);
    },
    draw() {
      gl.viewport(0, 0, canvas.width, canvas.height);
      gl.uniform1i(u.mode, { dots: 0, squint: 1, raw: 2 }[view.mode] ?? 0);
      gl.uniform1f(u.dotSize, view.dot);
      gl.uniform1f(u.bloom, view.bloom);
      gl.uniform1f(u.aa, 0.75 * W / canvas.width);
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    },
  };
}

// CSS pixels per LED for the chosen size.
function pitchPx() {
  if (view.size === 'actual') return view.pxPerMm * PITCH_MM;
  if (view.size !== 'fit') return Number(view.size) || 12;
  const stage = $('#stage').getBoundingClientRect();
  const chrome = 90; // bezel + stage padding
  return Math.max(3, Math.floor(Math.min((stage.width - chrome) / W, (stage.height - chrome) / H)));
}

function sizeCanvas(canvas) {
  const pitch = pitchPx();
  const dpr = window.devicePixelRatio || 1;
  canvas.style.width = `${W * pitch}px`;
  canvas.style.height = `${H * pitch}px`;
  canvas.width = Math.round(W * pitch * dpr);
  canvas.height = Math.round(H * pitch * dpr);
}

// ---------- small control helpers ----------

function bindSlider(root, { get, set, format }) {
  const input = root.querySelector('input');
  const out = root.querySelector('output');
  const show = () => { out.textContent = format(Number(input.value)); };
  input.value = get();
  show();
  input.addEventListener('input', () => { set(Number(input.value)); show(); });
  return { refresh() { if (document.activeElement !== input) { input.value = get(); } show(); } };
}

function bindRadios(root, { get, set }) {
  const inputs = [...root.querySelectorAll('input')];
  const refresh = () => inputs.forEach((i) => { i.checked = i.value === String(get()); });
  inputs.forEach((i) => i.addEventListener('change', () => i.checked && set(i.value)));
  refresh();
  return { refresh };
}

function bindSwitch(input, { get, set }) {
  const refresh = () => { input.checked = get(); };
  refresh();
  input.addEventListener('change', () => set(input.checked));
  return { refresh };
}

const pct = (v) => `${Math.round(v * 100)}%`;
const trim = (v, step) => v.toFixed(step >= 1 ? 0 : step >= 0.1 ? 1 : 2);
const nf = new Intl.NumberFormat();

function ago(seconds) {
  if (seconds === null || seconds === undefined) return 'never';
  if (seconds < 2) return 'just now';
  if (seconds < 90) return `${Math.round(seconds)} s ago`;
  if (seconds < 5400) return `${Math.round(seconds / 60)} min ago`;
  return `${Math.round(seconds / 3600)} h ago`;
}

function duration(seconds) {
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
const words = (s) => String(s).replace(/_/g, ' ');

const kb = (bytes) => `${Math.round(bytes / 1024)} KB`;

/** What the panel does when nothing is streaming, spelled out. The keys are
 *  `screeny_device_api::IdleMode`; anything else falls back to the raw name,
 *  so firmware that grows a mode says something rather than nothing. */
const IDLE = {
  status: 'shows its status screen',
  hold_forever: 'holds the last frame',
  dim: 'dims the last frame',
  black: 'goes black',
};

/** The WiFi line. A non-null address means the link is up whatever
 *  `wifi_state` says - see `wifi_stale_failure` in devices.rs. */
function wifiLine(f) {
  const where = f.ssid || 'no network';
  return `${where} · ${f.link_up ? `${f.rssi_dbm} dBm` : words(f.wifi_state)}`;
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

let noticeTimer = null;
function notice(message, tone) {
  const el = $('#notice');
  el.hidden = !message;
  el.textContent = message || '';
  if (tone) { el.dataset.tone = tone; } else { delete el.dataset.tone; }
  clearTimeout(noticeTimer);
  // A transient message goes away; a lost connection does not.
  if (message && tone === 'say') { noticeTimer = setTimeout(() => { el.hidden = true; }, 6000); }
}

// ---------- talking to the server ----------

// This browser, so the server can leave our own changes out of what it pushes
// back to us: adopting them would fight with the slider still under the mouse.
const CLIENT = crypto.randomUUID?.() ?? `c${Math.random().toString(36).slice(2)}`;

// Reads; everything else is a POST carrying its arguments as JSON.
const GETS = new Set(['bootstrap', 'frame', 'piece_playing', 'panel_status', 'status', 'devices']);

async function invoke(cmd, args) {
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

// One socket: binary messages are frames, text messages say what they are.
// It reconnects by itself, because the server is allowed to be restarted.
function connect(handlers) {
  const url = new URL('api/v1/ws', document.baseURI);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  url.searchParams.set('client', CLIENT);
  let wait = 250;
  const open = () => {
    const socket = new WebSocket(url);
    socket.binaryType = 'arraybuffer';
    socket.addEventListener('open', () => { wait = 250; notice(''); });
    socket.addEventListener('message', (e) => {
      if (e.data instanceof ArrayBuffer) { handlers.frame(e.data); return; }
      const message = JSON.parse(e.data);
      handlers[message.type]?.(message);
    });
    socket.addEventListener('close', () => {
      notice('Lost contact with the studio. Reconnecting…');
      setTimeout(open, wait);
      wait = Math.min(wait * 2, 5000);
    });
    socket.addEventListener('error', () => socket.close());
  };
  open();
}

// ---------- the studio ----------

async function start() {
  const canvas = $('#panel');
  const renderer = createRenderer(canvas);
  const boot = await invoke('bootstrap');
  let state = boot.state;
  /** The last GET /api/v1/status. */
  let picture = null;

  // Controls that show a value from `state`. Another browser changing
  // something is the same thing as this one doing it, so both paths end here.
  const refreshers = [];              // bound once, below
  let paramControls = [];             // rebuilt whenever the piece changes
  const bind = (control) => { refreshers.push(control); return control; };

  const call = (cmd, args) => invoke(cmd, args).catch((e) => { notice(`${cmd} failed: ${e.message || e}`, 'say'); return null; });
  const pushSettings = () => call('set_settings', { settings: state.settings });
  const pushPlayback = () => call('set_playback', { paused: state.paused, speed: state.speed, fps: state.fps });

  /** Run something that talks to the panel, and say what came of it. */
  async function attempt(what, fn) {
    try {
      const out = await fn();
      notice(typeof out === 'string' ? out : what, 'say');
      return out;
    } catch (e) {
      notice(`${what}: ${e.message || e}`);
      return null;
    } finally {
      refreshPicture();
    }
  }

  // ---- piece, seed, parameters ----

  const pieceById = Object.fromEntries(boot.pieces.map((p) => [p.id, p]));

  // Card 145: a GPU piece with no adapter renders black, and used to say so
  // only on the process's stderr - which in a container is `docker logs`,
  // which nobody is reading. The outcome is decided once by the server and
  // comes down in `bootstrap`.
  const gpu = boot.gpu || { available: true };
  const unplayable = (p) => Boolean(p && p.needs_gpu && !gpu.available);
  const blocked = boot.pieces.filter(unplayable);

  $('#pieces').replaceChildren(...boot.pieces.map((p) => {
    const label = document.createElement('label');
    const input = Object.assign(document.createElement('input'), { type: 'radio', name: 'piece', value: p.id });
    const span = Object.assign(document.createElement('span'), { textContent: p.name });
    if (unplayable(p)) {
      // Not offered, rather than offered and then black.
      input.disabled = true;
      label.dataset.unavailable = 'yes';
      label.title = 'Needs a graphics adapter, and there is none here.';
      span.append(Object.assign(document.createElement('em'), { textContent: 'no GPU' }));
    }
    input.addEventListener('change', async () => adopt(await call('set_piece', { id: p.id })));
    label.append(input, span);
    return label;
  }));

  $('#gpu-note').hidden = blocked.length === 0;
  if (blocked.length) {
    const names = blocked.map((p) => p.name).join(', ');
    $('#gpu-note').textContent = `${names} cannot be drawn here — ${gpu.error || 'no graphics adapter'}.`;
  }

  /** A black picture never passes silently: if the piece that is *already*
   *  loaded needs an adapter there is none for - which is how a state file
   *  from a machine with a GPU arrives in a container without one - the stage
   *  says so until another piece is picked.
   *
   *  It is re-asserted rather than said once, because the notice line is
   *  shared: the socket clears it when it (re)connects, and a transient
   *  message may be sitting on it. So this writes only when the line is free
   *  or already carries this message, and never pushes aside something
   *  somebody is reading. */
  let blackNotice = '';
  function sayIfBlack() {
    const piece = pieceById[state.piece];
    const el = $('#notice');
    if (!unplayable(piece)) {
      if (el.textContent === blackNotice) notice('');
      delete $('#gpu-note').dataset.tone;
      blackNotice = '';
      return;
    }
    $('#gpu-note').dataset.tone = 'bad';
    blackNotice = `${piece.name} needs a graphics adapter, so the panel is black — ${gpu.error || 'no graphics adapter'}. Pick another piece.`;
    if (el.hidden || el.textContent === blackNotice) notice(blackNotice);
  }

  /** One parameter's control (card 163).
   *
   *  A parameter is still an `f32` from end to end - wire, state file,
   *  per-piece memory - and every one of these sets it with `set_param`. What
   *  the spec now *declares* is what shape the thing is: an ordinary number is
   *  a slider, a list of named stops is a list, and off-or-on is a switch. A
   *  parameter whose values are a list used to be a slider with the key
   *  crammed into its label ("Resting dials (0: as it was, 1: quiet, ...)"),
   *  so the person moving it was reading a legend and counting stops.
   *
   *  Three or fewer stops are a segmented control, which fits across the
   *  inspector at 390 px. More than three - fourteen choreographies, nine
   *  moods - is a select: a segmented control would either wrap into a muddle
   *  or scroll sideways. */
  function paramControl(spec) {
    const id = `param-${spec.id}`;
    const set = (v) => { state.params[spec.id] = v; call('set_param', { id: spec.id, value: v }); };
    const value = () => state.params[spec.id];

    if (spec.switch) {
      const root = document.createElement('label');
      root.className = 'switch';
      const input = Object.assign(document.createElement('input'), { id, type: 'checkbox' });
      root.append(input, Object.assign(document.createElement('span'), { textContent: spec.label }));
      paramControls.push(bindSwitch(input, { get: () => value() >= 0.5, set: (on) => set(on ? 1 : 0) }));
      return root;
    }

    if (spec.choices && spec.choices.length) {
      return spec.choices.length <= 3 ? segmented(spec, id, value, set) : dropdown(spec, id, value, set);
    }

    const root = document.createElement('div');
    root.className = 'slider';
    const label = Object.assign(document.createElement('label'), { htmlFor: id, textContent: spec.label });
    const input = Object.assign(document.createElement('input'), {
      id, type: 'range', min: spec.min, max: spec.max, step: spec.step,
    });
    root.append(label, document.createElement('output'), input);
    paramControls.push(bindSlider(root, { get: value, set, format: (v) => trim(v, spec.step) }));
    return root;
  }

  /** A short list: one button per stop, like the panel-model controls. */
  function segmented(spec, id, value, set) {
    const root = document.createElement('fieldset');
    root.className = 'seg';
    root.id = id;
    root.append(Object.assign(document.createElement('legend'), { textContent: spec.label }));
    spec.choices.forEach((name, i) => {
      const label = document.createElement('label');
      const input = Object.assign(document.createElement('input'), { type: 'radio', name: id, value: String(i) });
      label.append(input, Object.assign(document.createElement('span'), { textContent: name }));
      root.append(label);
    });
    paramControls.push(bindRadios(root, { get: () => Math.round(value()), set: (v) => set(Number(v)) }));
    return root;
  }

  /** A long list: a select, which says the chosen name and can hold fourteen
   *  of them at any width. */
  function dropdown(spec, id, value, set) {
    const root = document.createElement('div');
    root.className = 'row row--choice';
    const select = Object.assign(document.createElement('select'), { id });
    select.append(...spec.choices.map((name, i) =>
      Object.assign(document.createElement('option'), { value: String(i), textContent: name })));
    root.append(Object.assign(document.createElement('label'), { htmlFor: id, textContent: spec.label }), select);
    select.addEventListener('change', () => set(Number(select.value)));
    paramControls.push({
      refresh() { if (!busy(select)) select.value = String(Math.round(value())); },
    });
    select.value = String(Math.round(value()));
    return root;
  }

  function adopt(next) {
    if (!next) return;
    state = next;
    const piece = pieceById[state.piece];
    $('#piece-name').textContent = piece ? piece.name : state.piece;
    $('#piece-blurb').textContent = piece ? piece.blurb : '';
    $('#ro-seed').textContent = state.seed;
    if (!busy($('#seed'))) $('#seed').value = state.seed;
    document.querySelectorAll('#pieces input').forEach((i) => { i.checked = i.value === state.piece; });

    paramControls = [];
    $('#params').replaceChildren(...(piece ? piece.params : []).map((spec) => paramControl(spec)));
    $('#reset-params').hidden = !piece || piece.params.length === 0;
    sayIfBlack();
    // Empty until the controls below are bound, which is the first call.
    for (const control of refreshers) control.refresh();
  }
  adopt(state);

  // A change another browser made: adopt it without rebuilding anything that
  // does not have to be rebuilt, so a slider being dragged here keeps its grip.
  function sync(next) {
    if (!next) return;
    if (next.piece !== state.piece) { adopt(next); return; }
    state = next;
    $('#ro-seed').textContent = state.seed;
    if (!busy($('#seed'))) $('#seed').value = state.seed;
    for (const control of [...refreshers, ...paramControls]) control.refresh();
  }

  const newSeed = async () => adopt(await call('set_seed', { seed: null }));
  $('#new-seed').addEventListener('click', newSeed);
  $('#seed').addEventListener('change', async (e) => {
    const seed = Math.max(0, Math.min(4294967295, Math.floor(Number(e.target.value) || 0)));
    adopt(await call('set_seed', { seed }));
  });
  $('#reset-params').addEventListener('click', async () => adopt(await call('reset_params')));

  // ---- time ----

  const pauseButton = $('#pause');
  const showPaused = () => {
    pauseButton.textContent = state.paused ? 'Play' : 'Pause';
    pauseButton.setAttribute('aria-pressed', String(state.paused));
  };
  const togglePause = () => { state.paused = !state.paused; showPaused(); pushPlayback(); };
  const restart = () => call('restart');
  pauseButton.addEventListener('click', togglePause);
  $('#restart').addEventListener('click', restart);
  showPaused();
  bind({ refresh: showPaused });
  // Card 172: any rate the player may be on, including one a script set. The
  // slider both shows it and changes it, and `set_playback` now clamps rather
  // than ignoring, so the two can no longer disagree.
  //
  // Not `bindSlider`: its output reads the input, and an input with whole
  // stops rounds a rate that has not got one. The readout says the rate the
  // player is really on; only the thumb is rounded, and never by more than
  // half a frame.
  const fpsInput = $('#fps');
  const fpsOut = $('#fps-slider').querySelector('output');
  const showFps = () => {
    const dragging = busy(fpsInput);
    if (!dragging) fpsInput.value = String(state.fps);
    const shown = dragging ? Number(fpsInput.value) : state.fps;
    fpsOut.textContent = `${Number.isInteger(shown) ? shown : shown.toFixed(1)} fps`;
  };
  fpsInput.addEventListener('input', () => { state.fps = Number(fpsInput.value); showFps(); pushPlayback(); });
  showFps();
  bind({ refresh: showFps });
  bind(bindSlider($('#speed-slider'), {
    get: () => state.speed,
    set: (v) => { state.speed = v; pushPlayback(); },
    format: (v) => `${v.toFixed(2)}×`,
  }));

  // ---- panel model ----

  const s = () => state.settings;
  bind(bindRadios($('#levels'), { get: () => s().levels, set: (v) => { s().levels = Number(v); pushSettings(); } }));
  bind(bindRadios($('#dither'), { get: () => s().dither, set: (v) => { s().dither = v; pushSettings(); } }));
  bind(bindSwitch($('#panel-model'), { get: () => s().panel_model, set: (v) => { s().panel_model = v; pushSettings(); } }));
  bind(bindSwitch($('#codec-preview'), { get: () => s().codec_preview, set: (v) => { s().codec_preview = v; pushSettings(); } }));

  // ---- limiter ----

  bind(bindSwitch($('#limiter-on'), {
    get: () => s().limiter.enabled,
    set: (v) => { s().limiter.enabled = v; pushSettings(); },
  }));
  bind(bindSlider($('#apl-slider'), {
    get: () => s().limiter.apl_cap,
    set: (v) => { s().limiter.apl_cap = v; pushSettings(); },
    format: pct,
  }));
  bind(bindSlider($('#rise-slider'), {
    get: () => s().limiter.max_rise_per_s,
    set: (v) => { s().limiter.max_rise_per_s = v; pushSettings(); },
    format: (v) => `${Math.round(1000 / v)} ms to full`,
  }));

  // ---- the panel ----
  //
  // Output on/off, brightness, the device's own controls, and - folded away,
  // because it is a setup action and not a daily one - which panel this is.

  const outSwitch = $('#panel-out');
  outSwitch.addEventListener('change', async () => {
    const want = outSwitch.checked;
    const out = await call('set_panel', want ? { on: true, to: '' } : { on: false });
    if (out) { state = out.state; showPanel(); }
    refreshPicture();
  });
  bind({ refresh: () => { if (!busy(outSwitch)) outSwitch.checked = Boolean(state.on); } });

  const bright = $('#bright');
  const brightOut = $('#bright-slider').querySelector('output');
  bright.addEventListener('input', () => {
    bright.value = String(snapBrightness(Number(bright.value)));
    brightOut.textContent = bright.value;
  });
  bright.addEventListener('change', () => {
    const device = attachedId();
    if (!device) { notice('No panel is attached, so there is no brightness to set.', 'say'); return; }
    const level = snapBrightness(Number(bright.value));
    attempt(`Brightness ${level}`, async () => {
      const out = await invoke('device/brightness', { device, level });
      return out && out.applied !== out.asked
        ? `This panel caps brightness at ${out.applied}.`
        : `Brightness ${out.applied}`;
    });
  });

  const attachedId = () => (picture ? picture.preview.device : state.device) || '';
  const attachedDevice = () => (picture ? picture.devices.find((d) => d.attached) : null) || null;
  /** The panel link, from the half-second heartbeat. Null when output is off. */
  let link = null;

  const needPanel = () => {
    const id = attachedId();
    if (!id) { notice('No panel is attached yet. Open "Change which panel" to pick one.', 'say'); }
    return id;
  };

  $('#identify').addEventListener('click', () => {
    const device = needPanel();
    if (device) attempt('Identifying', () => invoke('device/identify', { device, ms: 3000 }));
  });
  $('#rename').addEventListener('click', () => {
    const device = needPanel();
    if (!device) return;
    const now = $('#panel-name').textContent;
    const name = window.prompt('What should this panel be called?', now);
    if (name === null) return;
    attempt('Renamed', () => invoke('device/name', { device, name: name.trim() }));
  });
  $('#reboot').addEventListener('click', () => {
    const device = needPanel();
    if (!device) return;
    const name = $('#panel-name').textContent;
    if (!window.confirm(`Reboot ${name}? It will go dark for a few seconds and then come back playing this.`)) return;
    attempt(`Rebooting ${name}`, () => invoke('device/reboot', { device, confirm: true }));
  });

  $('#add').addEventListener('click', async () => {
    const to = $('#add-to').value.trim();
    if (!to) { notice('Type an address, a host name, or the panel’s name first.'); return; }
    const done = await attempt(`Using ${to}`, () => invoke('set_panel', { on: true, to }));
    if (done) { $('#add-to').value = ''; state = done.state; showPanel(); }
  });
  $('#look').addEventListener('click', () => attempt('Asked every panel who it is', () => invoke('devices/refresh', {})));

  /** The one line that answers "is it on the panel?".
   *
   *  It reads the half-second heartbeat rather than the two-second poll, so
   *  a panel going away shows up in half a second and the answer does not
   *  depend on a read that may not have happened yet. */
  function panelPill() {
    const player = (attachedDevice() || {}).player;
    if (!attachedId()) return ['No panel', 'away'];
    if (player && player.health.gave_up) return ['Stopped', 'bad'];
    if (!state.on) return ['Output off', 'away'];
    if (link && link.connected) return ['On the panel', 'on'];
    return ['Panel away', 'away'];
  }

  function showPanel() {
    const [label, tone] = panelPill();
    for (const el of [$('#ro-panel'), $('#panel-pill')]) {
      if (el.textContent !== label) el.textContent = label;
      el.dataset.state = tone;
    }
    $('#stage').dataset.panel = tone === 'on' ? 'on' : 'away';

    // The device list is polled, so it can legitimately be a moment behind
    // the state and the heartbeat. Nothing below may assume it is here.
    const d = attachedDevice();
    const player = d && d.player;
    const where = d ? (d.frame_addr || d.address || d.instance || d.id) : '';
    const name = d ? d.label : attachedId() || '';

    $('#panel-name').textContent = name || 'No panel yet';
    $('#panel-help').textContent = !attachedId()
      ? 'Nothing is being sent. The picture above is what the panel will show when one is found.'
      : !state.on
        ? 'The panel is on its own idle screen. The picture above is still playing here.'
        : link && link.connected
          ? `Sending to ${where || name}.`
          : `${name} is away. It will pick this up again by itself when it comes back.`;

    // Brightness. The maximum is the device's own ceiling once it has told us
    // what that is, so the slider cannot ask for something it will not give.
    const cap = (player && player.health.brightness_cap) || 255;
    if (bright.max !== String(cap)) bright.max = String(cap);
    const t = d && d.telemetry;
    const shown = t ? t.brightness : player && (player.health.brightness_applied ?? player.brightness);
    if (!busy(bright) && shown !== null && shown !== undefined) {
      bright.value = String(Math.min(shown, cap));
      brightOut.textContent = bright.value;
    }
    $('#bright-note').textContent = player && player.brightness !== null && player.brightness !== undefined
      ? `Kept at ${player.brightness} across reconnects.`
      : 'Not managed: whatever the panel has.';

    const rows = [];
    if (link && state.on) {
      rows.push(['Link', link.state + (link.connected ? ` · ${link.fps.toFixed(0)} fps` : ''), link.connected ? null : 'dim']);
      rows.push(['Frames', `${nf.format(link.frames_sent)} sent, ${nf.format(link.frames_coalesced)} folded, ${nf.format(link.frames_dropped)} lost`]);
      if (link.codec_name) {
        rows.push(['Last frame', `${link.codec_name}, ${link.bytes} B${link.exact ? ', exact' : ', requantised'}`]);
      }
      if (link.indexed_fallback) rows.push(['Requantised', nf.format(link.indexed_fallback), 'warn']);
    }
    if (player) {
      // Card 171: a player-lifetime count that survives the link being
      // rebuilt, not `sessions - 1` - which was per link object, and the
      // studio builds a new link whenever what it is aiming at changes.
      rows.push(['Reconnects', `${nf.format(player.health.reconnects)} since the studio started`]);
      rows.push(['Rendered', `${nf.format(player.health.ticks)} frames at ${player.fps_measured.toFixed(0)} fps`]);
      if (player.health.panics || player.health.stalls) {
        rows.push(['Faults', `${player.health.panics} panics, ${player.health.stalls} stalls, ${player.health.restarts} restarts`, 'warn']);
      }
    }
    // Card 180: the device's own account of itself, when the firmware serves
    // one. Where a row would be said twice - uptime, signal - it is said in
    // the Device block below and left out here, so nothing is repeated and a
    // panel with no HTTP API shows exactly what it always did.
    const f = d && d.facts;
    if (d) {
      rows.push(['Heard', ago(d.last_seen_ago), d.last_seen_ago > 60 ? 'dim' : null]);
      if (t) {
        rows.push(['Panel', `${t.state} · ${nf.format(t.frames_shown)} shown`]);
        if (!f) rows.push(['Up', duration(t.uptime_s)]);
        if (!f) rows.push(['Signal', `${t.rssi_dbm} dBm`, t.rssi_dbm < -75 ? 'warn' : null]);
        const drops = t.drops.stale + t.drops.superseded + t.drops.decode + t.drops.rejected;
        rows.push([
          'Dropped',
          drops === 0 && t.drops.seq_gaps === 0
            ? 'none'
            : `${t.drops.stale} stale, ${t.drops.superseded} superseded, ${t.drops.decode} decode, ${t.drops.rejected} rejected, ${t.drops.seq_gaps} gaps`,
          drops ? 'warn' : null,
        ]);
      }
      if (d.firmware) rows.push(['Firmware', d.firmware + (d.panel_size ? ` · ${d.panel_size}` : '')]);
      if (d.last_error) rows.push(['Last error', d.last_error, 'bad']);
    }
    facts($('#panel-facts'), rows);
    showDevice(d);

    // Card 181: the switch is still live with no panel attached, and it still
    // means something - `state.on` is what makes the first panel found start
    // playing without anybody pressing anything. What it cannot say while
    // there is no panel is "show it on the panel", because two lines above,
    // the page has just said nothing is being sent.
    const outLabel = attachedId() ? 'Show it on the panel' : 'Drive a panel as soon as one is found';
    if ($('#panel-out-label').textContent !== outLabel) $('#panel-out-label').textContent = outLabel;

    const looking = discoveryLine(Boolean(attachedId()));
    $('#discovery-note').textContent = looking;
    $('#discovery-note').hidden = !looking;
    // Card 145, on the half-second heartbeat: the notice line is shared, so
    // a black GPU piece says so again as soon as the line is free.
    sayIfBlack();

    // Card 167: remembered settings this build could not use as written. Not a
    // fault - it is what the memory is for - so it is said plainly, once.
    const repaired = picture ? picture.state.repaired : [];
    $('#panel-repairs').hidden = !repaired || repaired.length === 0;
    if (repaired && repaired.length) {
      $('#panel-repairs').textContent = `Settings put right on the way in: ${repaired.join('; ')}`;
    }

    showFound();
  }
  bind({ refresh: showPanel });

  /** Card 180: the panel's own account of itself, read by the server from
   *  GET /api/v1/status on the device and cached there - the browser never
   *  talks to the panel, which has one connection worker and would drop a
   *  second caller at SYN.
   *
   *  Absent is normal: firmware older than 0.4.0 serves no HTTP at all, and
   *  then this whole block is hidden and the page above is the page it was.
   *
   *  **Quiet by default.** Everything here is a plain number in the ordinary
   *  tone. The things meant to catch an eye are the ones that mean something
   *  happened to the panel rather than in it: a reset that was not a power-on
   *  or a reboot we asked for, a settings-store error, a firmware slot that is
   *  not valid, and memory running out. Which is which is decided once, on the
   *  server, beside the reasoning for the thresholds (`devices::STACK_WARN`,
   *  `devices::STACK_FAULT`, `devices::HIGH_HEAP`, measured on the real device
   *  by the firmware session) - never a number written out twice.
   *
   *  Card 195: free stack has two levels, because the margin going is worth a
   *  different noise from the margin being gone, and a reboot nobody here
   *  asked for is said quietly rather than as a fault: on this firmware a
   *  crash and a reflash are the same `reset_reason`. */
  function showDevice(d) {
    const f = d && d.facts;
    $('#device-block').hidden = !f;
    if (!f) { $('#device-note').hidden = true; return; }

    const rows = [
      ['Slot', `${words(f.fw_slot)} · ${words(f.fw_state)}`, f.bad_fw_state ? 'warn' : null],
      // uptime_ms wraps at 49.7 days, like telemetry's. So does the panel's.
      ['Up', duration(f.uptime_ms / 1000)],
      ['Memory', `${kb(f.heap_used)} of ${kb(f.heap_size)}`, f.low_heap ? 'bad' : null],
      // The fault first: a panel below it is below the warning line too.
      ['Free stack', `${nf.format(f.stack_free)} B`, f.stack_fault ? 'bad' : f.stack_warn ? 'warn' : null],
      ['WiFi', wifiLine(f), f.link_up ? (f.rssi_dbm < -75 ? 'warn' : null) : 'warn'],
      ['Reboots', rebootLine(f)],
      ['Last reset', words(f.reset_reason), f.odd_reset ? 'bad' : null],
    ];
    if (f.store_errors) rows.push(['Store errors', nf.format(f.store_errors), 'bad']);
    rows.push(['When idle', IDLE[f.idle_mode] || words(f.idle_mode)]);
    facts($('#device-facts'), rows);

    const notes = [];
    // Said once, under the block, rather than shouted in the row: the studio
    // knows the panel restarted and knows it did not ask, and that is all it
    // knows. Never a fault tone, and never in /healthz.
    if (f.unasked_reboots) {
      notes.push('A crash and a reflash look the same from here.');
    }
    // Until firmware card 223 `wifi_state` is the sticky result of the last
    // credentials attempt, not the link. A panel with an address is on the
    // network whatever that field says, so this is a note and never a fault.
    if (f.wifi_stale_failure) notes.push('The last WiFi change failed; it is still on the network it had.');
    if (f.portal) notes.push('Its setup portal is up.');
    if (d.facts_ago > 120) notes.push(`This is what it last said about itself, ${ago(d.facts_ago)}.`);
    $('#device-note').textContent = notes.join(' ');
    $('#device-note').hidden = notes.length === 0;
  }

  /** Card 195: the Reboots row.
   *
   *  The studio asks for exactly one kind of reboot - the Reboot button above -
   *  and writes down when it did. A restart it did not ask for is the honest
   *  "it may have crashed" this firmware can give: the chip cannot tell a
   *  panic from any other software reset, so the row says what is known and
   *  the note under the block says what it does not mean. Counted by the
   *  server, from `boot_id`, never from uptime. */
  function rebootLine(f) {
    if (!f.reboots) return 'none since the studio started';
    const since = `${nf.format(f.reboots)} since the studio started`;
    if (!f.unasked_reboots) return since;
    return `${since} · ${nf.format(f.unasked_reboots)} the studio did not ask for`;
  }

  /** Card 173: which of the three "nothing here yet" this is.
   *
   *  An empty list looks the same whether the first browse has simply not
   *  finished, mDNS is broken on this host, or discovery is switched off and
   *  nothing will ever appear. The first is "wait a moment" and the third is
   *  "you have to type something", so the page has to tell them apart. None of
   *  it is a fault: a browse that finds nothing is the normal case, so this is
   *  a hint and never reaches `/healthz`.
   *
   *  Returns '' when there is nothing worth saying, which is the ordinary
   *  state of an attached panel on a host where discovery works. */
  function discoveryLine(attached) {
    const d = picture && picture.discovery;
    if (!d) return '';
    const type = 'Type an address under “Change which panel”.';
    if (!d.enabled) {
      return attached
        ? 'Not looking for other panels: this studio was started with --no-discover.'
        : `Not looking for panels: this studio was started with --no-discover. ${type}`;
    }
    if (d.last_error) {
      return `Looking for panels is not working here: ${d.last_error}. ${type}`;
    }
    if (!d.browses) return 'Looking for panels…';
    if (attached) return '';
    const browses = `${d.browses} ${d.browses === 1 ? 'browse' : 'browses'}`;
    const found = (picture.devices || []).length;
    return found
      ? `Looking: ${browses}, ${found} found.`
      : `Looking: ${browses}, nothing found yet. ${type}`;
  }

  /** Every panel this studio knows about: the chooser, folded away. */
  function showFound() {
    const devices = picture ? picture.devices : [];
    const attached = attachedId();
    // With no panel at all, the chooser is the point of the page: open it.
    if (!attached && !$('#setup').open && !$('#setup').dataset.nudged) {
      $('#setup').open = true;
      $('#setup').dataset.nudged = 'yes';
    }
    $('#found').replaceChildren(...devices.map((d) => {
      const row = document.createElement('div');
      row.className = 'found__one';
      row.dataset.attached = d.id === attached ? 'yes' : 'no';
      const what = document.createElement('div');
      what.className = 'found__what';
      what.append(
        Object.assign(document.createElement('div'), { className: 'found__name', textContent: d.label }),
        Object.assign(document.createElement('div'), {
          className: 'found__where',
          textContent: d.frame_addr || d.address || d.instance || d.id,
        }),
      );
      row.append(what);
      if (d.id === attached) {
        row.append(Object.assign(document.createElement('span'), { className: 'pill', textContent: 'This one' }));
      } else {
        const use = Object.assign(document.createElement('button'), { type: 'button', textContent: 'Use this' });
        use.addEventListener('click', async () => {
          const out = await attempt(`Now showing ${d.label}`, () => invoke('set_panel', { on: true, to: d.id }));
          if (out) { state = out.state; adopt(state); showPanel(); }
        });
        row.append(use);
      }
      const forget = Object.assign(document.createElement('button'), { type: 'button', className: 'quiet', textContent: 'Forget' });
      forget.addEventListener('click', () => {
        if (!window.confirm(`Forget ${d.label}? Its settings go with it.`)) return;
        attempt(`Forgot ${d.label}`, () => invoke('devices/forget', { device: d.id }));
      });
      row.append(forget);
      return row;
    }));
  }

  // ---- view ----

  const resize = () => { sizeCanvas(canvas); renderer.draw(); };
  const showDensity = () => {
    $('#density-row').hidden = $('#density-hint').hidden = view.size !== 'actual';
  };
  const modeRadios = bindRadios($('#view-mode'), {
    get: () => view.mode,
    set: (v) => { view.mode = v; storeView(); renderer.draw(); },
  });
  bindRadios($('#view-size'), {
    get: () => view.size,
    set: (v) => { view.size = v; storeView(); showDensity(); resize(); },
  });
  $('#density').value = view.pxPerMm;
  $('#density').addEventListener('change', (e) => {
    view.pxPerMm = Math.max(2, Math.min(12, Number(e.target.value) || 4.96));
    storeView();
    resize();
  });
  bindSlider($('#dot-slider'), {
    get: () => view.dot,
    set: (v) => { view.dot = v; storeView(); renderer.draw(); },
    format: (v) => `${pct(v)} of pitch`,
  });
  bindSlider($('#bloom-slider'), {
    get: () => view.bloom,
    set: (v) => { view.bloom = v; storeView(); renderer.draw(); },
    format: pct,
  });
  showDensity();
  resize();
  new ResizeObserver(resize).observe($('#stage'));
  matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`).addEventListener('change', resize);

  // ---- keys ----

  window.addEventListener('keydown', (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    // A control that has focus owns its own keys: space on a button is that
    // button, not Pause, and `n` in the address box is an `n`.
    const tag = e.target instanceof HTMLElement ? e.target.tagName : '';
    if (['INPUT', 'BUTTON', 'SELECT', 'TEXTAREA', 'SUMMARY'].includes(tag)) return;
    const mode = { 1: 'dots', 2: 'squint', 3: 'raw' }[e.key];
    if (mode) { view.mode = mode; storeView(); modeRadios.refresh(); renderer.draw(); }
    else if (e.key === ' ') { e.preventDefault(); togglePause(); }
    else if (e.key === 'n') newSeed();
    else if (e.key === 'r') restart();
  });

  // ---- meters ----

  const meter = (id) => {
    const root = $(id);
    return {
      root,
      out: root.querySelector('output'),
      of: root.querySelector('.meter__of'),
      fill: root.querySelector('.fill'),
      ghost: root.querySelector('.ghost'),
      tick: root.querySelector('.tick'),
      note: root.querySelector('.meter__note'),
    };
  };
  const mColours = meter('#m-colours'), mBytes = meter('#m-bytes'), mApl = meter('#m-apl'), mDluma = meter('#m-dluma');
  const width = (el, frac) => { el.style.width = `${Math.max(0, Math.min(1, frac)) * 100}%`; };

  // Wire codec ids, spec 4. The studio shows the name the sender uses.
  const CODECS = { 0x02: 'pal5', 0x10: 'pal8-lz', 0x11: 'pal4-lz', 0x28: 'bc1-dual', 0x7f: 'solid' };

  function showStats(st) {
    mColours.out.textContent = st.colours;
    width(mColours.fill, st.colours / 64);
    mColours.root.dataset.state = st.exact ? '' : 'warn';
    mColours.note.textContent = st.exact
      ? 'Sent exactly: every pixel reaches the panel as drawn.'
      : 'Too many to send exactly. The encoder is quantising this frame.';

    mBytes.out.textContent = st.bytes;
    mBytes.of.textContent = `of ${boot.payload_bytes} bytes`;
    width(mBytes.fill, st.bytes / boot.payload_bytes);
    mBytes.root.dataset.state = st.exact ? '' : 'warn';
    // Measured, not estimated: this is what the encoder really produced.
    mBytes.note.textContent = `${CODECS[st.codec] || `codec ${st.codec}`}, measured.`;

    const cap = s().limiter.apl_cap;
    const limiting = st.gain < 0.995;
    mApl.out.textContent = pct(st.apl);
    mApl.of.textContent = `cap ${pct(cap)}`;
    width(mApl.fill, st.apl);
    mApl.ghost.style.left = `${Math.min(1, st.apl) * 100}%`;
    width(mApl.ghost, Math.min(1, st.aplIn) - Math.min(1, st.apl));
    mApl.tick.style.left = `${cap * 100}%`;
    mApl.root.dataset.state = limiting ? 'warn' : !s().limiter.enabled && st.apl > cap ? 'over' : '';
    mApl.note.textContent = limiting
      ? `The piece asked for ${pct(st.aplIn)}. Limiter is holding it at ×${st.gain.toFixed(2)}.`
      : 'Limiter idle.';

    // Bar spans 0..10% of full scale per frame; the tick is the limiter's rise rate.
    const perFrame = s().limiter.max_rise_per_s / (state.fps || 60);
    mDluma.out.textContent = `${(st.dluma * 100).toFixed(1)}%`;
    width(mDluma.fill, st.dluma / 0.1);
    mDluma.ghost.style.left = `${Math.min(1, st.dluma / 0.1) * 100}%`;
    width(mDluma.ghost, (st.dlumaPeak - st.dluma) / 0.1);
    mDluma.tick.style.left = `${Math.min(1, perFrame / 0.1) * 100}%`;
    mDluma.root.dataset.state = st.dlumaPeak > perFrame * 1.05 ? 'over' : '';
    mDluma.note.textContent = `Peak ${(st.dlumaPeak * 100).toFixed(1)}% in the last 2 s.`;

    $('#ro-time').textContent = `${st.t.toFixed(1)} s`;
    $('#ro-fps').textContent = `${st.fps.toFixed(1)} fps`;
  }

  // ---- now playing ----

  let playingKey = '';
  function showPlaying(p) {
    $('#playing-title').hidden = $('#playing-detail').hidden = !p;
    if (!p) {
      playingKey = '';
      $('#playing-actions').replaceChildren();
      $('#playing-notes').replaceChildren();
      return;
    }
    $('#playing-title').textContent = p.title;
    $('#playing-detail').textContent = p.detail;
    $('#playing-notes').replaceChildren(...p.notes.map((n) => Object.assign(document.createElement('li'), { textContent: n })));
    // Rebuild the buttons only when the set changes, so a click is never lost
    // to a refresh.
    const key = p.actions.map((a) => a.id).join();
    if (key === playingKey) return;
    playingKey = key;
    $('#playing-actions').replaceChildren(...p.actions.map((a) => {
      const b = Object.assign(document.createElement('button'), { type: 'button', textContent: a.label });
      b.dataset.id = a.id;
      b.addEventListener('click', () => call('piece_act', { action: a.id }));
      return b;
    }));
  }

  // ---- the panel's own facts, polled ----

  let statusTimer = null;
  async function refreshPicture() {
    try {
      picture = await invoke('status');
    } catch {
      return; // the socket's own reconnect notice covers this
    }
    showPanel();
  }
  function tick() {
    clearInterval(statusTimer);
    if (document.hidden) return;
    refreshPicture();
    statusTimer = setInterval(refreshPicture, STATUS_MS);
  }
  document.addEventListener('visibilitychange', tick);
  // One read whatever the tab is doing, so a page opened in a background tab
  // is already right the moment somebody looks at it. It is the *repeat* that
  // a hidden tab is spared: a phone in a pocket should not poll all night.
  refreshPicture();
  tick();

  // ---- the frame pump ----
  //
  // The server produces frames whether or not anything is watching and the
  // socket carries the newest one; we hold on to the last that arrived and
  // draw it on the next display refresh, so a tab that cannot keep up drops
  // frames on the floor rather than queueing them.
  let newest = null;
  let lastSeq = -1;
  let shown = 0;
  function draw() {
    const buf = newest;
    if (buf) {
      newest = null;
      const dv = new DataView(buf);
      const seq = dv.getUint32(0, true);
      if (seq !== lastSeq) {
        lastSeq = seq;
        renderer.upload(new Uint8Array(buf, HEADER, W * H * 3));
        renderer.draw();
        if (shown++ % 3 === 0) {
          showStats({
            t: dv.getFloat32(4, true),
            colours: dv.getUint32(8, true),
            bytes: dv.getUint32(12, true),
            codec: dv.getUint32(16, true),
            exact: dv.getUint32(20, true) !== 0,
            apl: dv.getFloat32(24, true),
            aplIn: dv.getFloat32(28, true),
            dluma: dv.getFloat32(36, true),
            dlumaPeak: dv.getFloat32(40, true),
            gain: dv.getFloat32(44, true),
            fps: dv.getFloat32(48, true),
          });
        }
      }
    }
    requestAnimationFrame(draw);
  }
  draw();

  // Everything the server pushes: frames, somebody else's changes, and the
  // half-second heartbeat that carries "now playing" and the panel link.
  connect({
    frame: (buf) => { newest = buf; },
    state: (message) => sync(message.state),
    status: (message) => { link = message.panel; showPlaying(message.playing); showPanel(); },
  });
}

start().catch((e) => notice(String(e.message || e)));
