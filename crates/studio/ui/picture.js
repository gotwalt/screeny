// The Studio's Picture screen: `/`.
//
// One channel, one picture. The server renders and encodes it once, and this
// page is a window onto it: the frames drawn here are the channel's encoded
// frames decoded - what every panel on it is sent - and every control changes
// the channel, which means every panel on it.
//
// Card 198 took the panel's own affairs - which panel, discovery, the link, the
// device's facts, identify/rename/reboot - to a screen of their own at
// `/panel`. Card 301 took the rest of what was not about the picture off this
// screen too - brightness, the panel model, the limiter, Speed, pause/restart
// and the seed's Another button, all now either on the Panel screen or gone
// (the owner: start/stop was baffling, and "zero people understand" the
// seed) - so what is left is the canvas, what is playing, its parameters, its
// named settings, and the **status chip** in the title block, which says what
// the panel is doing, wears a fault tone when it needs attention, and is the
// way to the Panel screen. View - how *this browser* draws the panel - is not
// a panel setting either, so it lives folded under the canvas instead of the
// sidebar.
//
// Frames, other browsers' changes and the half-second heartbeat arrive on one
// WebSocket; the panel's own facts come from GET /api/v1/status every couple of
// seconds while the tab is visible. Everything else is a POST to
// /api/v1/<command>.
//
// The page asks for only as many frames as it can use (`previewFps`), and for
// none at all while the tab is hidden. It never asks for a *different* picture:
// what arrives is the panel's own frames, paced.
//
// Card 354 (on card 353's server): the screen is about a **channel** - the
// picture: patch, setting, tweaks, output settings and the encoded frames,
// the same bytes for every panel on it. It opens on a row of channels
// (`channelRow`, common.js) - a thumbnail, a name, the picture and the panels
// on it as chips - and the one in the URL (`/?channel=<id>`, Channel 1
// without it) is in the editor below: its frames on the canvas, "On: Kitchen,
// Hallway" under the title, its Output settings (moved here from the Panel
// screen, because they shape every member's frames), and "Panels on this
// channel", where a panel is moved to another channel. Every change is sent
// with its `channel`. Card 351's join-another-panel and split-this-one-off
// buttons went with card 353's implicit channels.

'use strict';

import {
  $, ago, bindRadios, bindSlider, bindSwitch, bootstrapFor, busy, carryNav, channelHref, channelRow, chosenChannel,
  connect, forgetChoice, H, HEADER, HOME_CHANNEL, invoke, linkWords, notice, panelHref, panelNamer, pct, pollStatus,
  showPanelsChip, trim, W,
} from './common.js';
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
  const chosen = chosenChannel();
  const boot = await bootstrapFor({ channel: chosen });
  let state = boot.state;
  /** The last GET /api/v1/status. */
  let picture = null;
  /** The overview, `{"type":"panels"}` and `{"type":"channels"}`: every
   *  panel, first first, and every channel, Channel 1 first. */
  let panels = [];
  let channels = [];
  /** The channel this screen is about: the socket's own, which is the one in
   *  the URL or - without one - Channel 1. */
  const here = () => state.channel;

  // Controls that show a value from `state`. Another browser changing
  // something is the same thing as this one doing it, so both paths end here.
  const refreshers = [];              // bound once, below
  let paramControls = [];             // rebuilt whenever the patch changes
  const bind = (control) => { refreshers.push(control); return control; };

  // Every change names the channel it is for (card 353's `channel`).
  const forHere = (args) => ({ ...(args || {}), channel: here() });
  const call = (cmd, args) => invoke(cmd, forHere(args)).catch((e) => { notice(`${cmd} failed: ${e.message || e}`, 'say'); return null; });
  // `state.output` is the studio's (card 356): the limiter's numbers feed
  // the meters below. It is set on the Settings screen.
  const s = () => state.output;

  // ---- patch, parameters ----

  const patchById = Object.fromEntries(boot.patches.map((p) => [p.id, p]));

  // Card 145: a GPU patch with no adapter renders black, and used to say so
  // only on the process's stderr - which in a container is `docker logs`,
  // which nobody is reading. The outcome is decided once by the server and
  // comes down in `bootstrap`.
  const gpu = boot.gpu || { available: true };
  // Card 357: whether this machine can play a patch is the server's answer
  // (`playable`, `unplayable_reason`), not a rule written out here again.
  const unplayable = (p) => Boolean(p && p.playable === false);
  const blocked = boot.patches.filter(unplayable);
  const whyNot = (p) => (p && p.unplayable_reason) || gpu.error || 'no graphics adapter';

  $('#patches').replaceChildren(...boot.patches.map((p) => {
    const label = document.createElement('label');
    const input = Object.assign(document.createElement('input'), { type: 'radio', name: 'patch', value: p.id });
    const span = Object.assign(document.createElement('span'), { textContent: p.name });
    if (unplayable(p)) {
      // Not offered, rather than offered and then black.
      input.disabled = true;
      label.dataset.unavailable = 'yes';
      label.title = `Not available here: ${whyNot(p)}.`;
      span.append(Object.assign(document.createElement('em'), { textContent: gpu.available ? 'software' : 'no GPU' }));
    }
    input.addEventListener('change', async () => adopt(await call('set_patch', { id: p.id })));
    label.append(input, span);
    return label;
  }));

  $('#gpu-note').hidden = blocked.length === 0;
  if (blocked.length) {
    const names = blocked.map((p) => p.name).join(', ');
    const reasons = [...new Set(blocked.map(whyNot))].join('; ');
    $('#gpu-note').textContent = `${names} cannot be drawn here — ${reasons}.`;
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
    blackNotice = `${patch.name} cannot be drawn here, so the panel ${gpu.available ? 'would stutter' : 'is black'} — ${whyNot(patch)}. Pick another patch.`;
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
    document.querySelectorAll('#patches input').forEach((i) => { i.checked = i.value === state.patch; });

    paramControls = [];
    $('#params').replaceChildren(...(patch ? patch.params : []).map((spec) => paramControl(spec)));
    sayIfBlack();
    // Empty until the controls below are bound, which is the first call.
    for (const control of refreshers) control.refresh();
  }
  adopt(state);

  // A change another browser made: adopt it without rebuilding anything that
  // does not have to be rebuilt, so a slider being dragged here keeps its grip.
  function sync(next) {
    if (!next) return;
    if (next.patch !== state.patch || next.channel !== state.channel) { adopt(next); return; }
    state = next;
    for (const control of [...refreshers, ...paramControls]) control.refresh();
  }

  // ---- the channel: its row, its panels, its name (card 354) ----
  //
  // The channel is the picture; panels are only on it. The row at the top
  // says that at a glance - every channel, its thumbnail, and the panels on it
  // as chips - and this section is where a panel is moved, the channel is
  // renamed or deleted. Channel routes name their channel themselves, so they
  // go through `invoke` rather than `call` (which adds this one's).

  const nameOf = (id) => panelNamer(panels)(id);
  const channelName = () => state.channel_name || `Channel ${here()}`;
  /** This channel's summary from the overview, once it has arrived. */
  const mine = () => channels.find((c) => c.id === here()) || null;
  /** The panels on this channel, by device id, in the order they joined. */
  const members = () => (mine() ? mine().panels : state.panels) || [];
  /** Them as the overview has them; one it has not caught up with yet is
   *  said by its id rather than left out. */
  const memberSummaries = () => members().map((id) => panels.find((p) => p.device === id)
    || { device: id, name: id, on: state.on, connected: false, link: 'connecting' });

  const failed = (what) => (e) => { notice(`${what}: ${e.message || e}`, 'say'); return null; };

  async function newChannel() {
    const made = await invoke('channels/new', { from: here() }).catch(failed('New channel'));
    if (made) location.assign(channelHref('/', made.channel));
  }
  const row = channelRow($('#channels'), { current: here, onNew: newChannel });

  /** Move a panel to a channel - `'new'` makes one first, as a copy of this
   *  one, so moving to it is seamless - and follow it there if it is new. */
  async function moveTo(panel, target) {
    let channel = Number(target);
    if (target === 'new') {
      const made = await invoke('channels/new', { from: here() }).catch(failed('New channel'));
      if (!made) return;
      channel = made.channel;
    }
    const done = await invoke('panel/channel', { panel, channel }).catch(failed(`Moving ${nameOf(panel)}`));
    if (!done) return;
    notice(`${nameOf(panel)} is on ${done.channel_name} now.`, 'say');
    if (target === 'new') { location.assign(channelHref('/', channel)); return; }
    if (done.channel === here()) sync(done);
  }

  /** A `<select>` of every channel, this one chosen, and "New channel". */
  function channelOptions(select, chosen, { placeholder = '' } = {}) {
    const opts = [];
    if (placeholder) opts.push(Object.assign(document.createElement('option'), { value: '', textContent: placeholder }));
    for (const c of channels) {
      opts.push(Object.assign(document.createElement('option'), { value: String(c.id), textContent: c.name }));
    }
    opts.push(Object.assign(document.createElement('option'), { value: 'new', textContent: 'New channel' }));
    select.replaceChildren(...opts);
    select.value = String(chosen);
  }

  let membersKey = '';
  function showChannel() {
    const count = channels.length;
    const ids = members();
    const names = ids.map(nameOf);

    $('#channel-kicker').hidden = count <= 1;
    $('#channel-kicker').textContent = channelName();
    $('#channel-on').textContent = names.length ? `On: ${names.join(', ')}` : 'On no panel';
    $('#channel-on').dataset.empty = names.length ? 'no' : 'yes';
    $('#empty-note').hidden = names.length > 0;
    $('#empty-note').textContent = names.length ? '' : `No panel is on ${channelName()}. It still plays here, and everything you change is kept; move a panel onto it below.`;
    carryNav({ channel: here(), panel: ids[0] || '' });

    // The members, rebuilt only when what they say changes and never under
    // somebody's hand.
    const list = $('#members');
    const key = JSON.stringify([memberSummaries().map((p) => [p.device, p.name, p.link, p.fading]), channels.map((c) => [c.id, c.name]), panels.map((p) => [p.device, p.name, p.channel])]);
    if (key !== membersKey && !list.contains(document.activeElement) && !busy($('#bring'))) {
      membersKey = key;
      list.replaceChildren(...memberSummaries().map((p) => {
        const li = Object.assign(document.createElement('li'), { className: 'member' });
        const name = Object.assign(document.createElement('a'), { className: 'member__name', href: panelHref('/panel', p.device), textContent: p.name || p.device });
        const [said, tone] = linkWords(p.link);
        const state = Object.assign(document.createElement('span'), { className: 'member__state', textContent: p.fading ? `${said} · moving` : said });
        state.dataset.state = tone;
        const move = Object.assign(document.createElement('label'), { className: 'member__move' });
        const select = document.createElement('select');
        select.setAttribute('aria-label', `Move ${p.name || p.device} to`);
        channelOptions(select, here());
        select.addEventListener('change', () => moveTo(p.device, select.value));
        move.append(Object.assign(document.createElement('span'), { textContent: 'Move to' }), select);
        li.append(name, state, move);
        return li;
      }));

      // Every panel that is somewhere else, to bring here.
      const others = panels.filter((p) => p.channel !== here());
      $('#bring-row').hidden = others.length === 0;
      $('#bring').replaceChildren(
        Object.assign(document.createElement('option'), { value: '', textContent: 'Choose a panel…' }),
        ...others.map((p) => Object.assign(document.createElement('option'), { value: p.device, textContent: `${p.name || p.device} (on ${p.channel_name})` })),
      );
    }
    $('#members-empty').hidden = ids.length > 0;
    $('#members-empty').textContent = panels.length
      ? 'None yet.'
      : 'None yet. A panel the studio finds joins Channel 1; the Panel screen can add one by address.';

    const home = Boolean(mine() ? mine().home : here() === HOME_CHANNEL);
    $('#channel-delete').disabled = home;
    $('#channel-note').textContent = home
      ? `${channelName()} cannot be deleted: it is where new panels join.`
      : 'Deleting it moves its panels to Channel 1.';
  }
  bind({ refresh: showChannel });
  showChannel(); // from the bootstrap state, before the socket has said anything

  $('#bring').addEventListener('change', () => {
    const panel = $('#bring').value;
    if (panel) moveTo(panel, String(here()));
  });

  // Rename and delete, inline - no native dialogs - like the
  // settings control.
  const channelForm = $('#channel-name');
  const channelInput = $('#channel-name-input');
  const channelConfirm = $('#channel-confirm');
  const sayChannel = (message) => {
    $('#channel-error').hidden = !message;
    $('#channel-error').textContent = message || '';
  };
  const closeChannelForms = () => { channelForm.hidden = true; channelConfirm.hidden = true; };
  $('#channel-rename').addEventListener('click', () => {
    closeChannelForms();
    sayChannel('');
    channelInput.value = channelName();
    channelForm.hidden = false;
    channelInput.focus();
    channelInput.select();
  });
  channelForm.addEventListener('submit', async (e) => {
    e.preventDefault();
    sayChannel('');
    try {
      sync(await invoke('channels/rename', { channel: here(), name: channelInput.value }));
      closeChannelForms();
    } catch (err) {
      sayChannel(err.message || String(err));
    }
  });
  $('#channel-name-cancel').addEventListener('click', () => { closeChannelForms(); sayChannel(''); });
  channelInput.addEventListener('keydown', (e) => { if (e.key === 'Escape') { closeChannelForms(); sayChannel(''); } });
  $('#channel-delete').addEventListener('click', () => {
    closeChannelForms();
    sayChannel('');
    const n = members().length;
    $('#channel-confirm-what').textContent = n
      ? `Delete ${channelName()}? ${n === 1 ? 'Its panel moves' : `Its ${n} panels move`} to Channel 1.`
      : `Delete ${channelName()}?`;
    channelConfirm.hidden = false;
    $('#channel-confirm-yes').focus();
  });
  $('#channel-confirm-no').addEventListener('click', closeChannelForms);
  $('#channel-confirm-yes').addEventListener('click', async () => {
    try {
      await invoke('channels/delete', { channel: here() });
      location.assign('/');
    } catch (err) {
      sayChannel(err.message || String(err));
    }
  });

  // ---- settings (card 151) ----
  //
  // A patch has a **working copy** - what is playing - and named **settings**.
  // The working copy remembers which one it came from; whether it has been
  // moved since is the server's `modified`, computed rather than remembered, so
  // this page never has to keep a flag of its own in step with anything.
  //
  // Everything here is one POST that answers with the whole state, which is
  // also broadcast: a second browser sees a save, a load, a rename or a delete
  // at once, with no extra read.

  /** The one setting that is not stored: the patch's own defaults, speed 1.00x
   *  and a fixed seed. It is the server's `state::DEFAULT_SETTING` - one
   *  spelling, checked by `tests/ui.rs` - and it heads the list, which is what
   *  the old "Reset" button became. */
  const DEFAULT_SETTING = 'Default';

  const settingList = $('#setting-list');
  const settingMark = $('#setting-mark');
  const settingError = $('#setting-error');
  const nameForm = $('#setting-name');
  const nameInput = $('#setting-name-input');
  const nameLabel = $('#setting-name-label');
  const confirmDelete = $('#setting-confirm');
  const confirmWhat = $('#setting-confirm-what');

  /** Which inline form is open: `'save-as'`, `'rename'` or nothing. */
  let naming = null;

  /** The one line the settings control says things on - inline, beside the
   *  control, rather than on the shared notice line at the far end of the
   *  page. A refusal here is about the name somebody has just typed. */
  const saySetting = (message) => {
    settingError.hidden = !message;
    settingError.textContent = message || '';
  };

  /** One settings change: the server's answer is the whole state, so there is
   *  nothing to re-read, and a refusal is a sentence to show rather than a
   *  reason to make the page wrong. */
  async function settingCall(cmd, args) {
    saySetting('');
    try {
      sync(await invoke(cmd, forHere(args)));
      return true;
    } catch (e) {
      saySetting(e.message || String(e));
      return false;
    }
  }

  function closeName() { naming = null; nameForm.hidden = true; }
  function closeConfirm() { confirmDelete.hidden = true; }

  function openName(kind) {
    naming = kind;
    closeConfirm();
    saySetting('');
    nameLabel.textContent = kind === 'rename' ? 'Rename to' : 'Save as';
    nameInput.value = state.setting === DEFAULT_SETTING ? '' : state.setting;
    nameForm.hidden = false;
    nameInput.focus();
    nameInput.select();
  }

  function openConfirm() {
    closeName();
    saySetting('');
    confirmWhat.textContent = `Delete “${state.setting}”?`;
    confirmDelete.hidden = false;
    $('#setting-confirm-yes').focus();
  }

  /** The list is rebuilt only when the names change, so a save does not shut an
   *  open dropdown under somebody's hand. */
  let listKey = '';
  function showSetting() {
    const names = [DEFAULT_SETTING, ...(state.settings || [])];
    const key = names.join('\u0000');
    if (key !== listKey) {
      listKey = key;
      settingList.replaceChildren(...names.map((n) =>
        Object.assign(document.createElement('option'), { value: n, textContent: n })));
    }
    if (!busy(settingList)) settingList.value = state.setting;
    settingMark.hidden = !state.modified;

    const onDefault = state.setting === DEFAULT_SETTING;
    // Default is the patch's own: it can be loaded and saved *from*, never
    // written over, renamed or deleted. Saying so by shape rather than by
    // refusing after the fact.
    $('#setting-save').textContent = onDefault ? 'Save as…' : 'Save';
    $('#setting-saveas').hidden = onDefault;
    $('#setting-rename').disabled = onDefault;
    $('#setting-delete').disabled = onDefault;
    $('#setting-revert').disabled = !state.modified;
  }
  showSetting();
  bind({ refresh: showSetting });

  settingList.addEventListener('change', () => {
    closeName();
    closeConfirm();
    settingCall('settings/load', { name: settingList.value });
  });
  $('#setting-save').addEventListener('click', () => {
    // On Default, Save *is* Save as...: there is nothing to write over.
    if (state.setting === DEFAULT_SETTING) { openName('save-as'); return; }
    settingCall('settings/save', {});
  });
  $('#setting-saveas').addEventListener('click', () => openName('save-as'));
  $('#setting-rename').addEventListener('click', () => openName('rename'));
  $('#setting-delete').addEventListener('click', openConfirm);
  $('#setting-revert').addEventListener('click', () => {
    closeName();
    closeConfirm();
    settingCall('settings/load', { name: state.setting });
  });

  nameForm.addEventListener('submit', async (e) => {
    e.preventDefault();
    const name = nameInput.value;
    const done = naming === 'rename'
      ? await settingCall('settings/rename', { to: name })
      : await settingCall('settings/save', { name });
    if (done) closeName();
  });
  $('#setting-name-cancel').addEventListener('click', () => { closeName(); saySetting(''); });
  nameInput.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') { closeName(); saySetting(''); }
  });
  $('#setting-confirm-yes').addEventListener('click', async () => {
    if (await settingCall('settings/delete', {})) closeConfirm();
  });
  $('#setting-confirm-no').addEventListener('click', closeConfirm);

  // Card 301 took the Another button (and the seed with it) off the page:
  // "zero people understand it", the owner said. `set_seed` is still on the
  // API for a script to call; nothing in the browser calls it now.

  // ---- the panel, as far as this screen is concerned ----
  //
  // Card 301 took Speed and pause/restart off this screen, and brightness to
  // `/panel`; what is left here about the panels themselves is the chip below
  // and the channel's member list above.

  /** The link of the channel's first panel, from the half-second heartbeat.
   *  Null when output is off or there is no panel. */
  let link = null;

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
   *  panel going away shows up in half a second.
   *
   *  Card 307: the chip's own wording (name, "live · N fps", the fault
   *  phrase) is one function now, `showChip` in `common.js`, shared with the
   *  other screens so they cannot say different things about the same
   *  panel again. This screen is the one that has a rate to pass it.
   *
   *  Card 354: it is about every panel on this channel - one panel's own chip
   *  when there is one, "2 panels · 2 live" when there are more, and the fault
   *  tone for whichever of them needs attention. */
  function showChip() {
    const chip = $('#ro-panel');
    const devices = picture ? picture.devices : [];
    const on = memberSummaries();
    const said = showPanelsChip(chip, { panels: on, devices, link, rate: link ? link.fps : null, none: 'No panel on it' });
    const device = on.length === 1 ? devices.find((d) => d.id === on[0].device) : null;
    chip.title = device && device.last_seen_ago !== null && device.last_seen_ago !== undefined
      ? `Heard ${ago(device.last_seen_ago)}. The panel screen has the rest.`
      : 'The panel screen has the rest.';

    // The frame goes quiet when the panel is away; the picture stays lit,
    // because it is still the truth about what is playing (card 170).
    // A channel with no panel has no panel to be away: it stays lit.
    $('#stage').dataset.panel = on.length === 0 || said.tone === 'on' ? 'on' : 'away';

    // Card 145, on the half-second heartbeat: the notice line is shared, so a
    // black GPU patch says so again as soon as the line is free.
    sayIfBlack();
  }
  bind({ refresh: showChip });
  showChip();

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

  // Card 301 took Space (pause), `n` (Another) and `r` (Restart) with the
  // controls they drove: view mode is what is left with a shortcut.
  window.addEventListener('keydown', (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    // A control that has focus owns its own keys.
    const tag = e.target instanceof HTMLElement ? e.target.tagName : '';
    if (['INPUT', 'BUTTON', 'SELECT', 'TEXTAREA', 'SUMMARY', 'A'].includes(tag)) return;
    const mode = { 1: 'dots', 2: 'squint', 3: 'raw' }[e.key];
    if (mode) { view.mode = mode; storeView(); modeRadios.refresh(); renderer.draw(); }
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

    // Bar spans 0..10% of full scale per frame; the tick is the limiter's rise
    // rate. `state.fps` is the server's report of the one rate (card 161), so
    // the 30 is not written down here as well; the fallback is only for a
    // bootstrap from a build older than the field.
    const perFrame = s().limiter.max_rise_per_s / (state.fps || 30);
    mDluma.out.textContent = `${(st.dluma * 100).toFixed(1)}%`;
    width(mDluma.fill, st.dluma / 0.1);
    mDluma.ghost.style.left = `${Math.min(1, st.dluma / 0.1) * 100}%`;
    width(mDluma.ghost, (st.dlumaPeak - st.dluma) / 0.1);
    mDluma.tick.style.left = `${Math.min(1, perFrame / 0.1) * 100}%`;
    mDluma.root.dataset.state = st.dlumaPeak > perFrame * 1.05 ? 'over' : '';
    mDluma.note.textContent = `Peak ${(st.dlumaPeak * 100).toFixed(1)}% in the last 2 s.`;
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
    frame: (buf) => { newest = buf; row.frame(buf); },
    state: (message) => sync(message.state),
    status: (message) => { link = message.panel; showPlaying(message.playing); showChip(); },
    panels: (message) => { panels = message.panels || []; row.update(channels, panels); showChannel(); showChip(); },
    channels: (message) => { channels = message.channels || []; row.update(channels, panels); showChannel(); showChip(); },
    // The channel in the URL has been deleted: go to Channel 1.
    error: () => forgetChoice(),
  }, undefined, { channel: chosen });
}

start().catch((e) => notice(String(e.message || e)));
