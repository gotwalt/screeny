// The Studio's Schedule screen: `/schedule`.
//
// Card 302 built the server side - modes, a daily timetable, "hold until the
// next entry". This card is the page a person actually uses, read top to
// bottom the way the owner would ask about it: **what is due now and until
// when**, then **the timetable** that decides it, then **the modes** it
// plays. A mode is made by playing something the way you want it - on the
// Picture screen, on the Panel screen - and coming here to name it; this
// screen never lets you compose a patch, a setting or a brightness by hand,
// the same way the settings control never does either.
//
// No canvas here, so - like the Panel screen - it asks the socket for no
// frames (card 120).

'use strict';

import { $, bindSwitch, busy, connect, invoke, noFrames, notice, pollStatus, showChip as paintChip } from './common.js';

async function start() {
  const boot = await invoke('bootstrap');
  /** `crate::schedule::PageState`, flat: the player's state, `modes`,
   *  `schedule`, `mode`, `overridden`, `until`, `schedule_note`. */
  let state = boot.state;
  /** The last GET /api/v1/status. */
  let picture = null;
  /** The panel link, from the half-second heartbeat. Null when output is off. */
  let link = null;

  const patchById = Object.fromEntries(boot.patches.map((p) => [p.id, p]));
  /** Card 187's real stops; `[1]` is the lowest nonzero one - "the dimmest
   *  the panel can show" - the same number `snap_brightness` on the server
   *  rounds a mode's brightness to. */
  const stops = boot.brightness_stops;

  const attachedId = () => (picture ? picture.preview.device : state.device) || '';
  const attachedDevice = () => (picture ? picture.devices.find((d) => d.attached) : null) || null;

  /** The one thing this screen says about the panel: the same chip every
   *  screen carries, so trouble is never hidden behind this tab either.
   *
   *  Card 307: this used to be its own, thinner copy - device name and the
   *  fault phrase only, with no "live"/"away"/"off" - so it read a bare
   *  "screeny-4a00a4 ›" while the Picture screen's said "screeny-4a00a4 ·
   *  live · 30 fps". Both screens now paint through the one `showChip` in
   *  `common.js`, fed the same fields; this screen has no rate to show
   *  (no canvas, card 198/301), so it passes `rate: null`. */
  function showChip() {
    paintChip($('#ro-panel'), {
      attachedId: attachedId(), device: attachedDevice(), on: state.on, link, rate: null,
    });
  }

  // ---------------------------------------------------------- one call ----
  //
  // Every route here answers the whole page state, like the settings routes
  // do, so there is nothing to re-read after a change - only something to
  // show if it was refused. Named the same as panel.js's own wrapper, not
  // `invoke` itself, so the wiring test's route scanner still finds every one
  // of these (it looks for that name followed by an open paren and a quote,
  // which is also why this paragraph is careful not to spell it out).
  async function call(cmd, args) {
    try {
      state = await invoke(cmd, args);
      render();
      return true;
    } catch (e) {
      notice(e.message || String(e));
      return false;
    }
  }

  // --------------------------------------------------------------- now ----

  function showDue() {
    const line = $('#due-line');
    const note = $('#due-note');
    const actions = $('#due-actions');
    let text;
    let sub = '';
    let canResume = false;
    if (!state.schedule.enabled) {
      text = 'The schedule is off.';
      sub = 'Turn it on below to play modes by time of day.';
    } else if (!state.mode) {
      text = 'The schedule is on, but the timetable is empty.';
      sub = 'Add a time and a mode below.';
    } else if (state.overridden) {
      text = `Overridden until ${state.until}.`;
      sub = `${state.mode} is due again then.`;
      canResume = true;
    } else {
      text = state.until ? `${state.mode} until ${state.until}.` : `${state.mode}.`;
    }
    if (state.schedule_note) sub = sub ? `${sub} ${state.schedule_note}` : state.schedule_note;
    if (line.textContent !== text) line.textContent = text;
    if (note.textContent !== sub) note.textContent = sub;
    note.hidden = !sub;
    actions.hidden = !canResume;
  }

  $('#resume').addEventListener('click', async () => {
    if (await call('schedule/resume', {})) notice('Back to schedule.', 'say');
  });

  // ---------------------------------------------------------- timetable ----

  /** The `<select>` of mode names both the add row and every existing row
   *  share the same options for. Rebuilt only when the names change, so an
   *  open dropdown is not shut under somebody's hand (the same rule
   *  `picture.js`'s setting list follows). */
  function fillModeNames(select, names) {
    const key = names.join('\u0000');
    if (select.dataset.key !== key) {
      select.dataset.key = key;
      const value = select.value;
      select.replaceChildren(...names.map((n) => Object.assign(document.createElement('option'), { value: n, textContent: n })));
      if (names.includes(value)) select.value = value;
    }
  }

  function entryRow(entry, idx, names) {
    const row = document.createElement('div');
    row.className = 'timetable__row';
    const time = Object.assign(document.createElement('input'), { type: 'time', step: '60' });
    time.value = entry.at;
    const select = document.createElement('select');
    fillModeNames(select, names);
    select.value = entry.mode;
    const remove = Object.assign(document.createElement('button'), { type: 'button', className: 'quiet', textContent: 'Remove' });

    /** Replace this one entry and send the whole timetable, like
     *  `settings/save` sends the whole working copy - the schedule is
     *  small and this keeps one route rather than a second one for a
     *  single field. */
    const commit = (patch) => {
      const entries = state.schedule.entries.map((e, i) => (i === idx ? { ...e, ...patch } : e));
      call('schedule/set', { entries });
    };
    time.addEventListener('change', () => commit({ at: time.value }));
    select.addEventListener('change', () => commit({ mode: select.value }));
    remove.addEventListener('click', () => call('schedule/set', { entries: state.schedule.entries.filter((_, i) => i !== idx) }));

    row.append(time, select, remove);
    return row;
  }

  const scheduleOn = bindSwitch($('#schedule-on'), {
    get: () => state.schedule.enabled,
    set: (v) => call('schedule/set', { enabled: v }),
  });

  function showTimetable() {
    const names = (state.modes || []).map((m) => m.name);
    const list = $('#timetable');
    // Never redraw a row somebody is mid-edit on.
    if (![...list.querySelectorAll('input,select')].some(busy)) {
      list.replaceChildren(...state.schedule.entries.map((e, i) => entryRow(e, i, names)));
    }
    scheduleOn.refresh();
    fillModeNames($('#entry-mode'), names);
    const canAdd = names.length > 0;
    $('#entry-add').disabled = !canAdd;
    $('#entry-time').disabled = !canAdd;
    $('#entry-mode').disabled = !canAdd;
    $('#timetable-hint').hidden = canAdd;
    $('#timetable-hint').textContent = 'Save a mode below before adding it to the timetable.';
  }

  $('#entry-add').addEventListener('click', async () => {
    const at = $('#entry-time').value;
    const mode = $('#entry-mode').value;
    if (!at) { notice('Pick a time first.'); return; }
    if (!mode) { notice('Save a mode first.'); return; }
    if (await call('schedule/set', { entries: [...state.schedule.entries, { at, mode }] })) {
      $('#entry-time').value = '';
    }
  });

  // -------------------------------------------------------------- modes ----

  /** "flock · Lava · brightness 190", the way `wifiLine` and the found-panel
   *  rows in `panel.js` say several small facts as one line. */
  function modeDetail(m) {
    const patch = patchById[m.patch] ? patchById[m.patch].name : `${m.patch} (not in this build)`;
    const setting = m.setting === null || m.setting === undefined ? 'as left' : m.setting;
    let brightness;
    if (m.brightness === null || m.brightness === undefined) brightness = 'brightness unchanged';
    else if (m.brightness === 0) brightness = 'a dark panel';
    else if (stops.length > 1 && m.brightness <= stops[1]) brightness = 'the dimmest the panel can show';
    else brightness = `brightness ${m.brightness}`;
    return [patch, setting, brightness].join(' · ');
  }

  function modeRow(m) {
    const row = document.createElement('div');
    row.className = 'mode';
    const head = document.createElement('div');
    head.className = 'mode__head';
    head.append(Object.assign(document.createElement('span'), { className: 'mode__name', textContent: m.name }));
    const detail = Object.assign(document.createElement('p'), { className: 'mode__detail', textContent: modeDetail(m) });
    const actions = document.createElement('div');
    actions.className = 'mode__actions';

    const apply = Object.assign(document.createElement('button'), { type: 'button', textContent: 'Apply now' });
    apply.addEventListener('click', async () => { if (await call('mode/apply', { name: m.name })) notice(`Playing ${m.name} now.`, 'say'); });

    const rename = Object.assign(document.createElement('button'), { type: 'button', textContent: 'Rename' });
    rename.addEventListener('click', async () => {
      const to = window.prompt(`Rename “${m.name}” to:`, m.name);
      if (to === null) return;
      const trimmed = to.trim();
      if (!trimmed || trimmed === m.name) return;
      if (await call('modes/rename', { from: m.name, to: trimmed })) notice(`Renamed to ${trimmed}.`, 'say');
    });

    const at = state.schedule.entries.filter((e) => e.mode === m.name).map((e) => e.at);
    const del = Object.assign(document.createElement('button'), { type: 'button', textContent: 'Delete' });
    if (at.length) {
      del.disabled = true;
      del.title = `In the timetable at ${at.join(', ')}; take it out first.`;
    } else {
      del.addEventListener('click', async () => {
        if (!window.confirm(`Delete “${m.name}”? This cannot be undone.`)) return;
        if (await call('modes/delete', { name: m.name })) notice(`Deleted ${m.name}.`, 'say');
      });
    }

    actions.append(apply, rename, del);
    row.append(head, detail, actions);
    return row;
  }

  function showModes() {
    $('#modes').replaceChildren(...(state.modes || []).map(modeRow));
  }

  $('#mode-save-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    const input = $('#mode-save-name');
    const name = input.value.trim();
    if (!name) { notice('Type a name for the mode first.'); return; }
    if (await call('modes/save', { name })) {
      input.value = '';
      notice(`Saved ${name}.`, 'say');
    }
  });

  // ------------------------------------------------------------- render ----

  function render() {
    showChip();
    showDue();
    showTimetable();
    showModes();
  }

  // No control on this screen draws a picture, so the periodic poll is only
  // for the panel's own facts (the chip), not for anything asked to redraw
  // every tick - `render()` itself is driven by `state`, which the socket
  // keeps current.
  pollStatus((next) => { picture = next; showChip(); });

  connect({
    state: (message) => { state = message.state; render(); },
    status: (message) => { link = message.panel; showChip(); },
  }, noFrames);

  render();
}

start().catch((e) => notice(String(e.message || e)));
