// The Studio's Schedule screen: `/schedule`.
//
// Card 303 builds the real thing - playing patches on a timetable rather than
// by hand. This card only gives it a real URL, the shared nav and the status
// chip, so that landing here is landing somewhere real rather than a 404, and
// so that a panel needing attention is never hidden behind this tab either
// (card 198's rule, restated for a third screen).
//
// No canvas here, so - like the Panel screen - it asks the socket for no
// frames (card 120).

'use strict';

import { $, attention, connect, invoke, noFrames, notice, panelState, pollStatus } from './common.js';

async function start() {
  const boot = await invoke('bootstrap');
  let state = boot.state;
  /** The last GET /api/v1/status. */
  let picture = null;
  /** The panel link, from the half-second heartbeat. Null when output is off. */
  let link = null;

  const attachedId = () => (picture ? picture.preview.device : state.device) || '';
  const attachedDevice = () => (picture ? picture.devices.find((d) => d.attached) : null) || null;

  /** The one thing this screen says about the panel: the same chip every
   *  screen carries, so trouble is never hidden behind this tab either. */
  function showChip() {
    const device = attachedDevice();
    const here = panelState({ attached: Boolean(attachedId()), device, on: state.on, link });
    const name = device ? device.label : attachedId();
    const needs = attention(device);
    const label = here.key === 'none' ? 'No panel' : [name || 'Panel', needs].filter(Boolean).join(' · ');
    const chip = $('#ro-panel');
    if (chip.textContent !== label) chip.textContent = label;
    chip.dataset.state = needs ? 'bad' : here.tone;
  }

  // No control on this screen changes anything yet, so nothing needs the
  // immediate re-read `pollStatus` returns - only the periodic poll itself.
  pollStatus((next) => { picture = next; showChip(); });

  connect({
    state: (message) => { state = message.state; showChip(); },
    status: (message) => { link = message.panel; showChip(); },
  }, noFrames);

  showChip();
}

start().catch((e) => notice(String(e.message || e)));
