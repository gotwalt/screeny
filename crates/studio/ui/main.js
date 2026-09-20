// Screeny Studio front end. The Rust engine owns time and rendering; this file
// draws the newest frame as LEDs and edits the engine's state.

'use strict';

const W = 64, H = 32;
const HEADER = 52; // keep in step with studio/src/main.rs
const PITCH_MM = 3; // LED pitch: the lit area is 192 x 96 mm

const $ = (sel) => document.querySelector(sel);

// ---------- per-viewer view settings (never sent to the engine) ----------

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
  if (!gl) throw new Error('This webview has no WebGL 2, so the panel cannot be drawn.');

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
  return Math.max(4, Math.floor(Math.min((stage.width - chrome) / W, (stage.height - chrome) / H)));
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
  return { refresh() { input.value = get(); show(); } };
}

function bindRadios(root, { get, set }) {
  const inputs = [...root.querySelectorAll('input')];
  const refresh = () => inputs.forEach((i) => { i.checked = i.value === String(get()); });
  inputs.forEach((i) => i.addEventListener('change', () => i.checked && set(i.value)));
  refresh();
  return { refresh };
}

function bindSwitch(input, { get, set }) {
  input.checked = get();
  input.addEventListener('change', () => set(input.checked));
}

const pct = (v) => `${Math.round(v * 100)}%`;
const trim = (v, step) => v.toFixed(step >= 1 ? 0 : step >= 0.1 ? 1 : 2);

function notice(message) {
  const el = $('#notice');
  el.hidden = !message;
  el.textContent = message || '';
}

// ---------- the studio ----------

async function start(invoke) {
  const canvas = $('#panel');
  const renderer = createRenderer(canvas);
  const boot = await invoke('bootstrap');
  let state = boot.state;

  const call = (cmd, args) => invoke(cmd, args).catch((e) => notice(`${cmd} failed: ${e}`));
  const pushSettings = () => call('set_settings', { settings: state.settings });
  const pushPlayback = () => call('set_playback', { paused: state.paused, speed: state.speed, fps: state.fps });

  // Piece, seed, parameters
  const pieceById = Object.fromEntries(boot.pieces.map((p) => [p.id, p]));

  $('#pieces').replaceChildren(...boot.pieces.map((p) => {
    const label = document.createElement('label');
    const input = Object.assign(document.createElement('input'), { type: 'radio', name: 'piece', value: p.id });
    const span = Object.assign(document.createElement('span'), { textContent: p.name });
    input.addEventListener('change', async () => adopt(await call('set_piece', { id: p.id })));
    label.append(input, span);
    return label;
  }));

  function adopt(next) {
    if (!next) return;
    state = next;
    const piece = pieceById[state.piece];
    $('#piece-name').textContent = piece.name;
    $('#piece-blurb').textContent = piece.blurb;
    $('#ro-seed').textContent = state.seed;
    $('#seed').value = state.seed;
    document.querySelectorAll('#pieces input').forEach((i) => { i.checked = i.value === state.piece; });

    $('#params').replaceChildren(...piece.params.map((spec) => {
      const root = document.createElement('div');
      root.className = 'slider';
      const id = `param-${spec.id}`;
      const label = Object.assign(document.createElement('label'), { htmlFor: id, textContent: spec.label });
      const input = Object.assign(document.createElement('input'), {
        id, type: 'range', min: spec.min, max: spec.max, step: spec.step,
      });
      root.append(label, document.createElement('output'), input);
      bindSlider(root, {
        get: () => state.params[spec.id],
        set: (v) => { state.params[spec.id] = v; call('set_param', { id: spec.id, value: v }); },
        format: (v) => trim(v, spec.step),
      });
      return root;
    }));
    $('#reset-params').hidden = piece.params.length === 0;
  }
  adopt(state);

  const newSeed = async () => adopt(await call('set_seed', { seed: null }));
  $('#new-seed').addEventListener('click', newSeed);
  $('#seed').addEventListener('change', async (e) => {
    const seed = Math.max(0, Math.min(4294967295, Math.floor(Number(e.target.value) || 0)));
    adopt(await call('set_seed', { seed }));
  });
  $('#reset-params').addEventListener('click', async () => adopt(await call('reset_params')));

  // Time
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
  bindRadios($('#fps'), { get: () => state.fps, set: (v) => { state.fps = Number(v); pushPlayback(); } });
  bindSlider($('#speed-slider'), {
    get: () => state.speed,
    set: (v) => { state.speed = v; pushPlayback(); },
    format: (v) => `${v.toFixed(2)}×`,
  });

  // Panel model
  const s = () => state.settings;
  bindRadios($('#levels'), { get: () => s().levels, set: (v) => { s().levels = Number(v); pushSettings(); } });
  bindRadios($('#dither'), { get: () => s().dither, set: (v) => { s().dither = v; pushSettings(); } });
  bindSwitch($('#panel-model'), { get: () => s().panel_model, set: (v) => { s().panel_model = v; pushSettings(); } });
  bindSwitch($('#codec-preview'), { get: () => s().codec_preview, set: (v) => { s().codec_preview = v; pushSettings(); } });

  // Send to panel. Everything about the link lives in screeny_art::output; this
  // is a switch, a text box and a line of status.
  const panelSwitch = $('#panel-send'), panelTo = $('#panel-to'), panelNote = $('#panel-note');
  panelTo.value = localStorage.getItem('screeny.panel.to') || '';
  const pushPanel = async () => {
    localStorage.setItem('screeny.panel.to', panelTo.value);
    showPanel(await call('set_panel', { on: panelSwitch.checked, to: panelTo.value }));
  };
  panelSwitch.addEventListener('change', pushPanel);
  // Retarget on Enter rather than on every keystroke.
  panelTo.addEventListener('change', () => panelSwitch.checked && pushPanel());

  function showPanel(p) {
    if (!p) { panelNote.textContent = 'Off. Nothing is being sent.'; panelNote.dataset.state = ''; return; }
    const where = p.device || p.target;
    const head = p.connected
      ? `Sending to ${where} at ${p.fps.toFixed(0)} fps.`
      : `${p.state[0].toUpperCase()}${p.state.slice(1)}: ${where}.`;
    // frames_coalesced is not a fault: a 60 fps piece into a 30 fps panel
    // folds half its frames away by design. Fallbacks are the number to watch.
    const counts = `${p.frames_sent} sent, ${p.frames_coalesced} coalesced, ${p.frames_dropped} dropped.`
      + ` Exact ${p.indexed_exact}, fallback ${p.indexed_fallback}.`;
    panelNote.textContent = `${head} ${counts}${p.last_error ? ` Last error: ${p.last_error}` : ''}`;
    panelNote.dataset.state = p.connected ? '' : 'warn';
  }
  setInterval(async () => panelSwitch.checked && showPanel(await call('panel_status')), 1000);

  // Limiter
  bindSwitch($('#limiter-on'), {
    get: () => s().limiter.enabled,
    set: (v) => { s().limiter.enabled = v; pushSettings(); },
  });
  bindSlider($('#apl-slider'), {
    get: () => s().limiter.apl_cap,
    set: (v) => { s().limiter.apl_cap = v; pushSettings(); },
    format: pct,
  });
  bindSlider($('#rise-slider'), {
    get: () => s().limiter.max_rise_per_s,
    set: (v) => { s().limiter.max_rise_per_s = v; pushSettings(); },
    format: (v) => `${Math.round(1000 / v)} ms to full`,
  });

  // View
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

  // Keys
  window.addEventListener('keydown', (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.target instanceof HTMLInputElement && e.target.type === 'number') return;
    const mode = { 1: 'dots', 2: 'squint', 3: 'raw' }[e.key];
    if (mode) { view.mode = mode; storeView(); modeRadios.refresh(); renderer.draw(); }
    else if (e.key === ' ') { e.preventDefault(); togglePause(); }
    else if (e.key === 'n') newSeed();
    else if (e.key === 'r') restart();
  });

  // Meters
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

  // Now playing: pieces that compose as they go say what they are performing
  // and may offer a control or two. Polled gently; it changes every few seconds at most.
  let playingKey = '';
  function showPlaying(p) {
    $('#playing').hidden = !p;
    if (!p) { playingKey = ''; return; }
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
      b.addEventListener('click', async () => {
        showPlaying(await call('piece_act', { action: a.id }));
      });
      return b;
    }));
  }
  setInterval(async () => showPlaying(await call('piece_playing')), 500);

  // Frame pump. The engine produces 30 frames a second whether or not we ask;
  // we take the newest one each display refresh and skip it if it is not new.
  let lastSeq = -1;
  let shown = 0;
  async function pump() {
    try {
      const buf = await invoke('frame');
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
      notice('');
    } catch (e) {
      notice(`Lost contact with the engine: ${e}`);
    }
    requestAnimationFrame(pump);
  }
  pump();
}

const tauriInvoke = window.__TAURI__?.core?.invoke;
if (tauriInvoke) {
  start(tauriInvoke).catch((e) => notice(String(e.message || e)));
} else {
  notice('This page is the studio window. Start it with: cargo run -p screeny-studio');
}
