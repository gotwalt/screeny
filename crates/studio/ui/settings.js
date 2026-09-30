// The Studio's Settings screen: `/settings`.
//
// Card 311: the studio's own settings, starting with Home Assistant. The
// integration used to be set up from the container's environment (card 308);
// it is set up here now, and kept in the studio's state file. The password
// goes one way: the page sends it and is only ever told whether one is saved.
//
// No canvas here, so - like the Panel screen - it asks the socket for no
// frames (card 120).

'use strict';

import { $, pct, bindRadios, bindSlider, bindSwitch, busy, carryNav, chosenChannel, chosenPanel, connect, facts, invoke, noFrames, notice, pollStatus, showPanelsChip } from './common.js';

/** How often to ask how the connection is doing. It changes on its own - a
 *  broker going away, a reconnect - so this screen asks rather than waits. */
const HA_MS = 2000;

async function start() {
  /** The last GET /api/v1/status. */
  let picture = null;
  /** The overview, `{"type":"panels"}` (card 350): every panel, first first. */
  let panels = [];
  /** The last GET /api/v1/home_assistant: `crate::ha::HaView`. */
  let ha = await invoke('home_assistant');

  /** The studio's output settings (card 356): one, for every channel. The
   *  server reports it on every channel; the first is as good as any. */
  let output = (await invoke('channels')).channels[0].output;

  // Card 351/354: this screen is about the studio, not one channel or panel;
  // a `?channel=` or `?panel=` it was opened with is only carried on to the
  // other two screens.
  carryNav({ channel: chosenChannel(), panel: chosenPanel() });

  /** The same chip every screen carries, so trouble is never hidden behind
   *  this tab either - here about every panel at once (card 351), since
   *  nothing on this screen is about one of them. No canvas, so no rate. */
  function showChip() {
    showPanelsChip($('#ro-panel'), { panels, devices: picture ? picture.devices : [] });
  }

  // -------------------------------------------------------------- output ----
  //
  // Card 356: card 301's `pushOutput()` path, moved for the third time. The
  // panel model, the dither and the limiter shape every frame of every
  // channel, so there is one of them.

  const pushOutput = () => invoke('set_output', { output }).catch((e) => notice(`set_output failed: ${e.message || e}`, 'say'));
  const outputs = [
    bindRadios($('#panel-kind'), { get: () => output.panel, set: (v) => { output.panel = v; pushOutput(); } }),
    bindRadios($('#dither'), { get: () => output.dither, set: (v) => { output.dither = v; pushOutput(); } }),
    bindSwitch($('#panel-model'), { get: () => output.panel_model, set: (v) => { output.panel_model = v; pushOutput(); } }),
    bindSwitch($('#codec-preview'), { get: () => output.codec_preview, set: (v) => { output.codec_preview = v; pushOutput(); } }),
    bindSwitch($('#limiter-on'), { get: () => output.limiter.enabled, set: (v) => { output.limiter.enabled = v; pushOutput(); } }),
    bindSlider($('#apl-slider'), { get: () => output.limiter.apl_cap, set: (v) => { output.limiter.apl_cap = v; pushOutput(); }, format: pct }),
    bindSlider($('#rise-slider'), {
      get: () => output.limiter.max_rise_per_s,
      set: (v) => { output.limiter.max_rise_per_s = v; pushOutput(); },
      format: (v) => `${Math.round(1000 / v)} ms to full`,
    }),
  ];

  // ------------------------------------------------------ home assistant ----

  /** One line for how the connection is doing, in the page's voice. */
  function statusLine(view) {
    const s = view.status;
    switch (s.state) {
      case 'connected': return [`Connected. Home Assistant shows this studio as “${view.name}”.`, null];
      case 'connecting': return [`Connecting to ${view.host}…`, null];
      case 'failed': return [`Cannot connect: ${s.detail}. Trying again.`, 'bad'];
      // On, and the connection not started yet: it is about to be.
      default: return [view.enabled ? `Connecting to ${view.host}…` : 'Off.', null];
    }
  }

  /** Put a value in a field unless somebody is typing in it. */
  function fill(input, value) {
    if (!busy(input) && input.value !== String(value)) input.value = value;
  }

  function showHa() {
    haOn.refresh();
    const [line, tone] = statusLine(ha);
    const el = $('#ha-status');
    if (el.textContent !== line) el.textContent = line;
    if (tone) { el.dataset.tone = tone; } else { delete el.dataset.tone; }
    fill($('#ha-host'), ha.host);
    fill($('#ha-port'), ha.port);
    fill($('#ha-user'), ha.username);
    fill($('#ha-name'), ha.name);
    fill($('#ha-id'), ha.instance);
    fill($('#ha-prefix'), ha.discovery_prefix);
    // The saved password is never sent here; say whether there is one.
    $('#ha-pass').placeholder = ha.password_set ? 'saved - type to change' : 'none';
    facts($('#ha-facts'), [
      ['Discovery', ha.discovery_topic],
      ['Topics', `${ha.topic_base}/…`],
    ]);
    $('#ha-forget').disabled = !ha.host;
  }

  /** Every route here answers the whole view, so there is nothing to re-read
   *  after a change - only something to show if it was refused. */
  async function change(args) {
    try {
      ha = await invoke('home_assistant/set', args);
      showHa();
      soon();
      return true;
    } catch (e) {
      notice(e.message || String(e));
      showHa();
      return false;
    }
  }

  const haOn = bindSwitch($('#ha-on'), {
    get: () => ha.enabled,
    // Switching on with the fields as typed, so "type the broker, flip the
    // switch" works without a Save in between.
    set: (v) => change({ ...fields(), enabled: v }),
  });

  /** The form as typed. The password only when something was typed: an empty
   *  box means "keep the saved one", since the page never has it. */
  function fields() {
    const out = {
      host: $('#ha-host').value.trim(),
      username: $('#ha-user').value.trim(),
      name: $('#ha-name').value.trim(),
      instance: $('#ha-id').value.trim(),
      discovery_prefix: $('#ha-prefix').value.trim(),
    };
    const port = Number($('#ha-port').value);
    if (Number.isInteger(port) && port > 0) out.port = port;
    const pass = $('#ha-pass').value;
    if (pass) out.password = pass;
    return out;
  }

  $('#ha-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    if (await change(fields())) {
      $('#ha-pass').value = '';
      notice('Saved.', 'say');
    }
  });

  $('#ha-forget').addEventListener('click', async () => {
    if (!window.confirm('Remove Screeny from Home Assistant? Its device and entities go, and any automation that uses them stops working until you switch this back on.')) return;
    $('#ha-forget').disabled = true;
    try {
      ha = await invoke('home_assistant/forget', {});
      notice('Removed from Home Assistant.', 'say');
    } catch (e) {
      notice(e.message || String(e));
    }
    showHa();
  });

  async function readHa() {
    if (document.hidden) return;
    try {
      ha = await invoke('home_assistant');
      showHa();
    } catch { /* the socket's own reconnect notice covers this */ }
  }
  setInterval(readHa, HA_MS);
  // A change takes a moment to connect or fail; ask again sooner than the
  // poll would, and at once when the tab comes back into view.
  const soon = () => { setTimeout(readHa, 500); setTimeout(readHa, 1500); };
  document.addEventListener('visibilitychange', readHa);

  // ------------------------------------------------------------- render ----

  pollStatus((next) => { picture = next; if (!panels.length) panels = next.panels || []; showChip(); });

  connect({
    panels: (message) => { panels = message.panels || []; showChip(); },
    // Another browser changed the output: adopt it, but not under a hand.
    channels: (message) => {
      const next = (message.channels || [])[0];
      if (next && JSON.stringify(next.output) !== JSON.stringify(output)) { output = next.output; outputs.forEach((o) => o.refresh()); }
    },
  }, noFrames);

  showChip();
  showHa();
}

start().catch((e) => notice(String(e.message || e)));
