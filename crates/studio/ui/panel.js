// The Studio's Panel screen: `/panel`.
//
// Everything about the panel itself and about this studio - which panel,
// whether it is being looked for, the link, what the device says about its own
// heap and WiFi and firmware, identify, rename, reboot, and the studio's own
// health. Card 198 moved it off the Picture screen so that the screen the art
// is judged on is about the art.
//
// It is the same state and the same stream as the Picture screen: a change
// made here shows there, and in another browser, at once. What it does **not**
// ask for is frames (card 120): there is no canvas on this screen, so every
// frame sent to it would be received and thrown away. `noFrames` is the pace
// it opens the socket with; the state messages and the half-second heartbeat
// arrive exactly as before.

'use strict';

import {
  $, ago, bindBrightness, busy, connect, duration, facts, IDLE, invoke, kb, kbs,
  makeAttempt, netSize, nf, noFrames, notice, panelState, pollStatus, size, wifiLine, words,
} from './common.js';

async function start() {
  const boot = await invoke('bootstrap');
  let state = boot.state;
  /** The last GET /api/v1/status. */
  let picture = null;
  /** The panel link, from the half-second heartbeat. Null when output is off. */
  let link = null;

  const patchById = Object.fromEntries(boot.patches.map((p) => [p.id, p]));

  const attachedId = () => (picture ? picture.preview.device : state.device) || '';
  const attachedDevice = () => (picture ? picture.devices.find((d) => d.attached) : null) || null;

  const attempt = makeAttempt(() => readStatus());

  // ---- output, brightness, the device's own controls ----

  const outSwitch = $('#panel-out');
  outSwitch.addEventListener('change', async () => {
    const want = outSwitch.checked;
    const done = await invoke('set_panel', want ? { on: true, to: '' } : { on: false })
      .catch((e) => { notice(`set_panel failed: ${e.message || e}`, 'say'); return null; });
    if (done) { state = done.state; showPanel(); }
    readStatus();
  });

  const brightness = bindBrightness({
    input: $('#bright'),
    out: $('#bright-slider').querySelector('output'),
    note: $('#bright-note'),
    attached: attachedId,
    attempt,
  });

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

  // ---- what is on the panel ----

  /** The title block: which panel this is, what it is doing, and - in words,
   *  because there is no canvas here - what it is playing. */
  function showHead(here, device) {
    const name = device ? device.label : attachedId();
    $('#panel-name').textContent = name || 'No panel yet';
    $('#panel-help').textContent = !attachedId()
      ? 'Nothing is being sent. The studio is still playing the patch; the Picture screen shows it.'
      : !state.on
        ? 'The panel is on its own idle screen. The patch is still playing here.'
        : link && link.connected
          ? `Sending to ${device ? (device.frame_addr || device.address || device.instance || device.id) : name}.`
          : `${name} is away. It will pick this up again by itself when it comes back.`;

    const pill = $('#panel-pill');
    if (pill.textContent !== here.label) pill.textContent = here.label;
    pill.dataset.state = here.tone;

    const patch = patchById[state.patch];
    $('#ro-playing').textContent = patch ? patch.name : state.patch;
    $('#ro-seed').textContent = state.seed;
  }

  function showPanel() {
    const device = attachedDevice();
    const here = panelState({ attached: Boolean(attachedId()), device, on: state.on, link });
    showHead(here, device);

    if (!busy(outSwitch)) outSwitch.checked = Boolean(state.on);
    // Card 181: the switch is still live with no panel attached, and it still
    // means something - `state.on` is what makes the first panel found start
    // playing without anybody pressing anything. What it cannot say while
    // there is no panel is "show it on the panel", because the line above has
    // just said nothing is being sent.
    const outLabel = attachedId() ? 'Show it on the panel' : 'Drive a panel as soon as one is found';
    if ($('#panel-out-label').textContent !== outLabel) $('#panel-out-label').textContent = outLabel;

    const looking = discoveryLine(Boolean(attachedId()));
    $('#discovery-note').textContent = looking;
    $('#discovery-note').hidden = !looking;

    brightness.show(device);
    showLink(device);
    showDevice(device);
    showStudio();
    showFound();
  }

  /** What the studio sees of the link and the player: the device list is
   *  polled, so it can legitimately be a moment behind the state and the
   *  heartbeat, and nothing here may assume it is there. */
  function showLink(d) {
    const player = d && d.player;
    const rows = [];
    if (link && state.on) {
      rows.push(['Link', link.state + (link.connected ? ` · ${link.fps.toFixed(0)} fps` : ''), link.connected ? null : 'dim']);
      rows.push(['Frames', `${nf.format(link.frames_sent)} sent, ${nf.format(link.frames_coalesced)} folded, ${nf.format(link.frames_dropped)} lost`]);
      if (link.codec_name) {
        rows.push(['Last frame', `${link.codec_name}, ${link.bytes} B${link.exact ? ', exact' : ', requantised'}`]);
      }
      if (link.indexed_fallback) rows.push(['Requantised', nf.format(link.indexed_fallback), 'warn']);
    }
    if (d && d.traffic) rows.push(...networkRows(d.traffic));
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
    const t = d && d.telemetry;
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
    if (!rows.length) rows.push(['Link', attachedId() ? 'nothing yet' : 'no panel attached', 'dim']);
    facts($('#panel-facts'), rows);
  }

  /** Card 164: what this panel costs the network.
   *
   *  Three rows: the rate, the split by path under it in the quiet tone, and
   *  the totals since the studio started - "2.1 GB out" being the number that
   *  actually answers "what is this costing my network".
   *
   *  **Every figure here was worked out on the server** (`devices.rs`), once
   *  per supervisor tick, so two browsers cannot show two different rates and
   *  this file never divides one counter by another. It is a number and not a
   *  chart on purpose; if a chart is wanted that is a card of its own.
   *
   *  What is counted: frames (UDP, the stream and the telemetry that comes
   *  back along it), control (UDP, the polls and the panel's own controls) and
   *  http (TCP, the panel's status API every ten seconds). The UDP figures
   *  carry 28 B a datagram for the IP and UDP headers; the HTTP one is the
   *  bytes on the socket and nothing more, because TCP's retransmissions and
   *  ACKs are not visible from up here. mDNS and the broadcast probe are not
   *  traffic with *a panel* and are left out - see the README. */
  function networkRows(t) {
    const r = t.rate || {};
    const secs = t.window_s || 5;
    return [
      ['Network', `${kbs(r.out)} out · ${kbs(r.in)} in`],
      [
        'By path',
        `frames ${kbs(r.frames_out, false)} · control ${kbs(r.control_out, false)} · `
        + `http ${kbs(r.http_out, false)} KB/s out, averaged over ${secs} s`,
        'dim',
      ],
      ['Sent', `${netSize(t.total.out.bytes)} out · ${netSize(t.total.in.bytes)} in since the studio started`],
    ];
  }

  /** Card 180: the panel's own account of itself, read by the server from
   *  GET /api/v1/status on the device and cached there - the browser never
   *  talks to the panel, which has one connection worker and would drop a
   *  second caller at SYN.
   *
   *  Absent is normal: firmware older than 0.4.0 serves no HTTP at all, and
   *  then this whole block is hidden and what is above it is what it was.
   *
   *  **Quiet by default.** Everything here is a plain number in the ordinary
   *  tone. The things meant to catch an eye are the ones that mean something
   *  happened to the panel rather than in it: a reset that was not a power-on
   *  or a reboot we asked for, an error in the device's own settings store,
   *  a firmware slot that is not valid, and memory running out. Which is
   *  which is decided once, on the server, beside the reasoning for the
   *  thresholds (`devices::STACK_WARN`, `devices::STACK_FAULT`,
   *  `devices::HIGH_HEAP`, measured on the real device by the firmware
   *  session) - never a number written out twice.
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

  /** Card 198: the studio's own health, which `/api/v1/status` has always
   *  carried and no page ever showed. It is here rather than on the Picture
   *  screen for the same reason everything else on this screen is: it is about
   *  the machinery, not the art.
   *
   *  `ok` is the same judgement `/healthz` makes, so this is the one place a
   *  person can see *why* a container is unhealthy without curl. Nothing else
   *  here is a fault: no adapter is not a fault (card 145), and neither is a
   *  studio that keeps its state in memory. */
  /** Seconds since a unix time the server gave us, by this browser's clock.
   *  Good enough for "17 s ago"; the two clocks are not synchronised and this
   *  never pretends otherwise by showing a wall time. */
  const nowAgo = (unix) => Math.max(0, Date.now() / 1000 - unix);

  function showStudio() {
    if (!picture) return;
    const gpu = picture.gpu || {};
    const store = picture.state || {};
    const sockets = picture.sockets || {};
    const rows = [
      ['Health', picture.ok ? 'ok' : (picture.problems || []).join('; ') || 'not ok', picture.ok ? null : 'bad'],
      ['Version', `${picture.version} · up ${duration(picture.uptime_s)}`],
      ['Graphics', gpu.available ? `${gpu.adapter}${gpu.backend ? ` · ${gpu.backend}` : ''}` : (gpu.error || 'no adapter'),
        gpu.available ? null : 'dim'],
      ['State', store.persisting
        ? `${store.path} · ${nf.format(store.writes)} writes${store.last_write_unix ? `, last ${ago(nowAgo(store.last_write_unix))}` : ''}`
        : `${store.path} · not written to disk`,
        store.last_error ? 'bad' : store.persisting ? null : 'dim'],
      ['Browsers', `${sockets.open} open, ${sockets.watching} being sent pictures`],
      ['Sent to browsers', `${nf.format(sockets.frames_sent)} frames · ${size(sockets.bytes_sent)}`],
    ];
    if (store.last_error) rows.push(['Last write', store.last_error, 'bad']);
    if (store.recovered) rows.push(['Started from', store.recovered, 'warn']);
    facts($('#studio-facts'), rows);

    const repaired = store.repaired || [];
    $('#state-repairs').hidden = repaired.length === 0;
    if (repaired.length) {
      $('#state-repairs').textContent = `Put right on the way in: ${repaired.join('; ')}`;
    }
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
    // With no panel at all, the chooser is the point of the screen: open it.
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
          if (out) { state = out.state; showPanel(); }
        });
        row.append(use);
      }
      const forget = Object.assign(document.createElement('button'), { type: 'button', className: 'quiet', textContent: 'Forget' });
      forget.addEventListener('click', () => {
        if (!window.confirm(`Forget ${d.label}? Its player goes with it.`)) return;
        attempt(`Forgot ${d.label}`, () => invoke('devices/forget', { device: d.id }));
      });
      row.append(forget);
      return row;
    }));
  }

  // ---- the studio's and the panel's facts, polled ----

  const readStatus = pollStatus((next) => { picture = next; showPanel(); });

  // The same stream the Picture screen is on - a change made there shows here
  // at once - but with `noFrames` for a pace: this screen draws no pictures,
  // so it asks for none (card 120).
  connect({
    state: (message) => { state = message.state; showPanel(); },
    status: (message) => { link = message.panel; showPanel(); },
  }, noFrames);

  showPanel();
}

start().catch((e) => notice(String(e.message || e)));
