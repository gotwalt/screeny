// The Studio's Picture screen: `/`.
//
// One panel, one picture. The server renders on the panel's own player and this
// page is a window onto it: the frames drawn here are the same decoded
// datagrams the panel is being sent, and every control changes the panel.
//
// Card 198 took the panel's own affairs - which panel, discovery, the link, the
// device's facts, identify/rename/reboot - to a screen of their own at
// `/panel`, so that this one is about the picture and nothing else. What is
// left of the panel here is two things, both of which change how the *picture*
// is judged: **brightness**, which is how the patch looks on the LEDs, and the
// **status chip** in the title block, which says what the panel is doing, wears
// a fault tone when it needs attention, and is the way to the Panel screen.
//
// Frames, other browsers' changes and the half-second heartbeat arrive on one
// WebSocket; the panel's own facts come from GET /api/v1/status every couple of
// seconds while the tab is visible. Everything else is a POST to
// /api/v1/<command>.
//
// The page asks for only as many frames as it can use (`previewFps`), and for
// none at all while the tab is hidden. It never asks for a *different* picture:
// what arrives is the panel's own frames, paced.

'use strict';

import {
  $, ago, attention, bindBrightness, bindRadios, bindSlider, bindSwitch, busy, connect,
  drawStops, invoke, makeAttempt, notice, panelState, pct, pollStatus, trim,
} from './common.js';

const W = 64, H = 32;
const HEADER = 52; // keep in step with studio/src/page.rs
const PITCH_MM = 3; // LED pitch: the lit area is 192 x 96 mm

// ---------- per-viewer view options (never sent to the server) ----------

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
  let paramControls = [];             // rebuilt whenever the patch changes
  const bind = (control) => { refreshers.push(control); return control; };

  const call = (cmd, args) => invoke(cmd, args).catch((e) => { notice(`${cmd} failed: ${e.message || e}`, 'say'); return null; });
  const pushOutput = () => call('set_output', { output: state.output });
  const pushPlayback = () => call('set_playback', { paused: state.paused, speed: state.speed, fps: state.fps });

  // ---- patch, seed, parameters ----

  const patchById = Object.fromEntries(boot.patches.map((p) => [p.id, p]));

  // Card 145: a GPU patch with no adapter renders black, and used to say so
  // only on the process's stderr - which in a container is `docker logs`,
  // which nobody is reading. The outcome is decided once by the server and
  // comes down in `bootstrap`.
  const gpu = boot.gpu || { available: true };
  const unplayable = (p) => Boolean(p && p.needs_gpu && !gpu.available);
  const blocked = boot.patches.filter(unplayable);

  $('#patches').replaceChildren(...boot.patches.map((p) => {
    const label = document.createElement('label');
    const input = Object.assign(document.createElement('input'), { type: 'radio', name: 'patch', value: p.id });
    const span = Object.assign(document.createElement('span'), { textContent: p.name });
    if (unplayable(p)) {
      // Not offered, rather than offered and then black.
      input.disabled = true;
      label.dataset.unavailable = 'yes';
      label.title = 'Needs a graphics adapter, and there is none here.';
      span.append(Object.assign(document.createElement('em'), { textContent: 'no GPU' }));
    }
    input.addEventListener('change', async () => adopt(await call('set_patch', { id: p.id })));
    label.append(input, span);
    return label;
  }));

  $('#gpu-note').hidden = blocked.length === 0;
  if (blocked.length) {
    const names = blocked.map((p) => p.name).join(', ');
    $('#gpu-note').textContent = `${names} cannot be drawn here — ${gpu.error || 'no graphics adapter'}.`;
  }

  /** A black picture never passes silently: if the patch that is *already*
   *  loaded needs an adapter there is none for - which is how a state file
   *  from a machine with a GPU arrives in a container without one - the stage
   *  says so until another patch is picked.
   *
   *  It is re-asserted rather than said once, because the notice line is
   *  shared: the socket clears it when it (re)connects, and a transient
   *  message may be sitting on it. So this writes only when the line is free
   *  or already carries this message, and never pushes aside something
   *  somebody is reading. */
  let blackNotice = '';
  function sayIfBlack() {
    const patch = patchById[state.patch];
    const el = $('#notice');
    if (!unplayable(patch)) {
      if (el.textContent === blackNotice) notice('');
      delete $('#gpu-note').dataset.tone;
      blackNotice = '';
      return;
    }
    $('#gpu-note').dataset.tone = 'bad';
    blackNotice = `${patch.name} needs a graphics adapter, so the panel is black — ${gpu.error || 'no graphics adapter'}. Pick another patch.`;
    if (el.hidden || el.textContent === blackNotice) notice(blackNotice);
  }

  /** One parameter's control (card 163).
   *
   *  A parameter is still an `f32` from end to end - wire, state file,
   *  per-patch memory - and every one of these sets it with `set_param`. What
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
    const patch = patchById[state.patch];
    $('#patch-name').textContent = patch ? patch.name : state.patch;
    $('#patch-blurb').textContent = patch ? patch.blurb : '';
    $('#ro-seed').textContent = state.seed;
    if (!busy($('#seed'))) $('#seed').value = state.seed;
    document.querySelectorAll('#patches input').forEach((i) => { i.checked = i.value === state.patch; });

    paramControls = [];
    $('#params').replaceChildren(...(patch ? patch.params : []).map((spec) => paramControl(spec)));
    $('#reset-params').hidden = !patch || patch.params.length === 0;
    sayIfBlack();
    // Empty until the controls below are bound, which is the first call.
    for (const control of refreshers) control.refresh();
  }
  adopt(state);

  // A change another browser made: adopt it without rebuilding anything that
  // does not have to be rebuilt, so a slider being dragged here keeps its grip.
  function sync(next) {
    if (!next) return;
    if (next.patch !== state.patch) { adopt(next); return; }
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
  // Card 183: every slider that declares its useful stops draws them. Static
  // markup, so once is enough.
  document.querySelectorAll('.slider').forEach(drawStops);
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
  const speedInput = $('#speed');
  const speed = bind(bindSlider($('#speed-slider'), {
    get: () => state.speed,
    set: (v) => { state.speed = v; pushPlayback(); },
    format: (v) => `${v.toFixed(2)}×`,
  }));
  // Card 197: 1.00x - "play it as the patch intended" - is one of 79 positions
  // and the only way back to it was the keyboard. It is a drawn stop now (the
  // datalist, card 183's `drawStops`) and a **double-click** goes home, which
  // is the way back the card asked for. The stops still do not snap: a magnet
  // at 1.00 would make 0.95 and 1.05 unreachable with a mouse, and a speed a
  // script set must be shown exactly.
  speedInput.addEventListener('dblclick', () => {
    speedInput.value = '1';
    state.speed = 1;
    pushPlayback();
    speed.show();
  });

  // ---- panel model ----

  const s = () => state.output;
  bind(bindRadios($('#panel-kind'), { get: () => s().panel, set: (v) => { s().panel = v; pushOutput(); } }));
  bind(bindRadios($('#dither'), { get: () => s().dither, set: (v) => { s().dither = v; pushOutput(); } }));
  bind(bindSwitch($('#panel-model'), { get: () => s().panel_model, set: (v) => { s().panel_model = v; pushOutput(); } }));
  bind(bindSwitch($('#codec-preview'), { get: () => s().codec_preview, set: (v) => { s().codec_preview = v; pushOutput(); } }));

  // ---- limiter ----

  bind(bindSwitch($('#limiter-on'), {
    get: () => s().limiter.enabled,
    set: (v) => { s().limiter.enabled = v; pushOutput(); },
  }));
  bind(bindSlider($('#apl-slider'), {
    get: () => s().limiter.apl_cap,
    set: (v) => { s().limiter.apl_cap = v; pushOutput(); },
    format: pct,
  }));
  bind(bindSlider($('#rise-slider'), {
    get: () => s().limiter.max_rise_per_s,
    set: (v) => { s().limiter.max_rise_per_s = v; pushOutput(); },
    format: (v) => `${Math.round(1000 / v)} ms to full`,
  }));

  // ---- the panel, as far as this screen is concerned ----

  const attachedId = () => (picture ? picture.preview.device : state.device) || '';
  const attachedDevice = () => (picture ? picture.devices.find((d) => d.attached) : null) || null;
  /** The panel link, from the half-second heartbeat. Null when output is off. */
  let link = null;

  const attempt = makeAttempt(() => readStatus());
  const brightness = bindBrightness({
    input: $('#bright'),
    out: $('#bright-slider').querySelector('output'),
    note: $('#bright-note'),
    attached: attachedId,
    attempt,
  });

  /** The status chip in the title block: the one thing on this screen that is
   *  about the panel rather than the picture, and the way to the Panel screen.
   *
   *  It says the panel's name, what it is doing and - while it is live - the
   *  rate it is being sent, so that "is it on the panel, and keeping up?" is
   *  answered without leaving the picture. It takes the **fault tone** when the
   *  panel needs attention (`attention()`, every flag of it the server's own
   *  judgement), so that trouble is never hidden behind a tab: that is the one
   *  thing a second screen could have cost, and it is the reason this chip
   *  exists at all.
   *
   *  It reads the half-second heartbeat rather than the two-second poll, so a
   *  panel going away shows up in half a second. */
  function showChip() {
    const device = attachedDevice();
    const here = panelState({ attached: Boolean(attachedId()), device, on: state.on, link });
    const name = device ? device.label : attachedId();
    const doing = here.key === 'live' && link
      ? `live · ${link.fps.toFixed(0)} fps`
      : here.key === 'off' ? 'output off'
        : here.key === 'away' ? 'away'
          : here.key === 'stopped' ? 'stopped' : '';
    const needs = attention(device);
    const label = here.key === 'none'
      ? 'No panel'
      : [name || 'Panel', doing, needs].filter(Boolean).join(' · ');
    const chip = $('#ro-panel');
    if (chip.textContent !== label) chip.textContent = label;
    chip.dataset.state = needs ? 'bad' : here.tone;
    chip.title = device && device.last_seen_ago !== null && device.last_seen_ago !== undefined
      ? `Heard ${ago(device.last_seen_ago)}. The panel screen has the rest.`
      : 'The panel screen has the rest.';

    // The frame goes quiet when the panel is away; the picture stays lit,
    // because it is still the truth about what is playing (card 170).
    $('#stage').dataset.panel = here.tone === 'on' ? 'on' : 'away';

    brightness.show(device);
    // Card 145, on the half-second heartbeat: the notice line is shared, so a
    // black GPU patch says so again as soon as the line is free.
    sayIfBlack();
  }
  bind({ refresh: showChip });

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
    // button, and `n` in a text box is an `n`.
    const tag = e.target instanceof HTMLElement ? e.target.tagName : '';
    if (['INPUT', 'BUTTON', 'SELECT', 'TEXTAREA', 'SUMMARY', 'A'].includes(tag)) return;
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
    // Card 102. Colour count is not the thing that decides exactness, so this
    // meter no longer pretends it is: 32 is the size that is exact whatever
    // the indices do (the fixed-rate rung), up to 256 is exact when the index
    // image compresses, and `st.exact` is the encoder's own answer for the
    // frame in hand. Spatial coherence is what costs bytes - a 200-colour
    // smooth gradient fits where 40 colours of confetti does not - so the note
    // says which of those three situations this is.
    mColours.out.textContent = st.colours;
    width(mColours.fill, st.colours / 256);
    mColours.root.dataset.state = st.exact ? '' : 'warn';
    mColours.note.textContent = !st.exact
      ? 'Not exact: too many colours for the structure in this frame, so the encoder is requantising it.'
      : st.colours <= 32
        ? 'Exact, and guaranteed: up to 32 colours fit whatever the pixels do.'
        : 'Exact: the index image compressed. Up to 256 can, when the picture is coherent.';

    mBytes.out.textContent = st.bytes;
    mBytes.of.textContent = `of ${boot.payload_bytes} bytes`;
    width(mBytes.fill, st.bytes / boot.payload_bytes);
    mBytes.root.dataset.state = st.exact ? '' : 'warn';
    // Measured, not estimated: this is the codec the sender's own chooser
    // picked and the size it really produced, for this frame.
    mBytes.note.textContent = `${CODECS[st.codec] || `codec ${st.codec}`}, measured${st.exact ? '' : ', lossy'}.`;

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
      ? `The patch asked for ${pct(st.aplIn)}. Limiter is holding it at ×${st.gain.toFixed(2)}.`
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
      b.addEventListener('click', () => call('patch_act', { action: a.id }));
      return b;
    }));
  }

  // ---- the panel's own facts, polled ----

  const readStatus = pollStatus((next) => { picture = next; showChip(); });

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
    status: (message) => { link = message.panel; showPlaying(message.playing); showChip(); },
  });
}

start().catch((e) => notice(String(e.message || e)));
