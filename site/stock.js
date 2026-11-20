// Back up and restore a Tidbyt's whole flash from the browser (Web Serial, esptool-js),
// and check an LED Art Panel's chip before its image is offered.
//
// Tidbyt does not publish its firmware, so the copy saved here is the only way back to
// the original. Every chunk read is checked against the chip's own MD5 of that range and
// read again on a mismatch: long reads through the Tidbyt's CP2102N have corrupted at
// higher rates, which is also why everything here runs at 230400 baud.
//
// The page offers one image per board, and ESP Web Tools cannot tell the boards apart
// (both are a plain ESP32 to it), so the flash size is the guard: a Tidbyt Gen 1 has
// 8 MB and its backup refuses anything else; an LED Art Panel has 4 MB, and its image is
// offered only after "Check" has found 4 MB. That check is what keeps the art panel's
// path (no backup, full erase) away from a Tidbyt's original firmware.
import { ESPLoader, Transport } from "./vendor/esptool-js/bundle.js";

const FLASH_BYTES = 8 * 1024 * 1024; // Tidbyt Gen 1: ESP32 with 8 MB of flash
const CHUNK = 256 * 1024;
const TRIES = 4;
const BAUD = 230400;

// What the page knows about each board. `manifest` is the ESP Web Tools manifest the
// Install button uses; the Tidbyt's keeps the original name so existing links hold.
const BOARDS = {
  "tidbyt": { manifest: "manifest.json" },
  "art-panel": { manifest: "manifest-art-panel.json", flash: "4MB", minRev: 1 },
};
let board = "tidbyt";

const $ = (id) => document.getElementById(id);
let busy = false;

function say(el, text, cls) {
  el.textContent = text;
  el.className = "status" + (cls ? " " + cls : "");
}

// Install stays off until there is a backup, or the owner says they do not need one
// (Tidbyt), or until the check has found an art panel's chip (LED Art Panel).
function setInstallAllowed(ok) {
  $("install-go").disabled = !ok || busy;
  $("install").classList.toggle("locked", !ok);
}

function setBusy(b) {
  busy = b;
  $("backup-go").disabled = b;
  $("check-go").disabled = b;
  $("restore-go").disabled = b || !$("restore-file").files.length;
  for (const r of document.querySelectorAll("input[name=board]")) r.disabled = b;
  setInstallAllowed(board === "tidbyt" ? backedUp || $("skip").checked : artChecked);
}

async function connect(log) {
  if (!("serial" in navigator)) throw new Error("This browser cannot talk to USB serial devices. Use desktop Chrome, Edge or Opera.");
  const port = await navigator.serial.requestPort();
  const transport = new Transport(port, false);
  const terminal = { clean() {}, writeLine(s) { log(s); }, write(s) { log(s); } };
  const loader = new ESPLoader({ transport, baudrate: BAUD, romBaudrate: BAUD, terminal });
  try {
    const chip = await loader.main();
    if (!/ESP32(?!-)/.test(chip)) throw new Error(`This is an ${chip}, not the ESP32 in a Tidbyt Gen 1.`);
    const size = await loader.detectFlashSize();
    if (size && size !== "8MB") throw new Error(`This chip has ${size} of flash; a Tidbyt Gen 1 has 8MB.`);
    const mac = await loader.chip.readMac(loader);
    return { loader, transport, mac };
  } catch (e) {
    await transport.disconnect().catch(() => {});
    throw e;
  }
}

async function release({ loader, transport }) {
  try { await loader.after("hard_reset"); } catch (e) { /* the port may already be gone */ }
  await transport.disconnect().catch(() => {});
}

function contains(hay, needle) {
  const n = new TextEncoder().encode(needle);
  outer: for (let i = 0; i <= hay.length - n.length; i++) {
    for (let j = 0; j < n.length; j++) if (hay[i + j] !== n[j]) continue outer;
    return true;
  }
  return false;
}

// What a whole-flash image holds: a bootloader at 0x1000 and a partition table at 0x8000.
function looksLikeFlash(img) {
  return img.length === FLASH_BYTES && img[0x1000] === 0xe9 && img[0x8000] === 0xaa && img[0x8001] === 0x50;
}

let backedUp = false;
let artChecked = false;

async function backup() {
  const status = $("backup-status");
  const bar = $("backup-bar");
  setBusy(true);
  let dev = null;
  try {
    say(status, "Pick the Tidbyt's port (CP2102 or USB Serial)...");
    dev = await connect((s) => console.log("[backup]", s));
    const unit = dev.mac.replace(/:/g, "").toLowerCase();
    const img = new Uint8Array(FLASH_BYTES);
    bar.hidden = false;
    const t0 = Date.now();
    for (let addr = 0; addr < FLASH_BYTES; addr += CHUNK) {
      let ok = false;
      for (let attempt = 1; attempt <= TRIES && !ok; attempt++) {
        const data = await dev.loader.readFlash(addr, CHUNK, (_p, got) => {
          const done = addr + got;
          bar.value = done / FLASH_BYTES;
          const secs = (Date.now() - t0) / 1000;
          const left = Math.max(0, Math.round((secs / done) * (FLASH_BYTES - done) / 60));
          say(status, `Reading ${(done / 1048576).toFixed(1)} of 8 MB, about ${left} min left. Keep the Tidbyt plugged in.`);
        });
        const want = await dev.loader.flashMd5sum(addr, CHUNK);
        ok = data.length === CHUNK && md5hex(data) === want;
        if (ok) img.set(data, addr);
        else console.warn(`[backup] chunk at 0x${addr.toString(16)} did not verify (try ${attempt})`);
      }
      if (!ok) throw new Error(`The read at 0x${addr.toString(16)} kept failing its check. Try another cable or USB port.`);
    }
    await release(dev);
    dev = null;
    if (!looksLikeFlash(img)) throw new Error("The copy was read cleanly but does not look like an ESP32 flash image. It was not saved.");

    const name = `tidbyt-${unit}-flash-backup.bin`;
    save(img, name);
    const already = contains(img, "screeny-fw");
    backedUp = true;
    say(status, already
      ? `Saved ${name}. Note: this Tidbyt already runs screeny, so this is a copy of screeny, not of Tidbyt's original firmware.`
      : `Saved ${name} (8 MB). Keep it somewhere safe: it is the only way back to Tidbyt's firmware on this unit.`, "ok");
  } catch (e) {
    say(status, friendly(e), "bad");
  } finally {
    if (dev) await release(dev);
    bar.hidden = true;
    setBusy(false);
  }
}

// An LED Art Panel has no original firmware to keep (its maker's sketch is rebuilt from
// source), so instead of a backup it gets a quick look at the chip: a classic ESP32 of
// revision 1 or later (the image's minimum) with 4 MB of flash. Reads nothing else.
async function checkArtPanel() {
  const status = $("check-status");
  const want = BOARDS["art-panel"];
  artChecked = false;
  setBusy(true);
  let loader = null, transport = null;
  try {
    if (!("serial" in navigator)) throw new Error("This browser cannot talk to USB serial devices. Use desktop Chrome, Edge or Opera.");
    say(status, "Pick the panel's port...");
    const port = await navigator.serial.requestPort();
    transport = new Transport(port, false);
    const log = (s) => console.log("[check]", s);
    const terminal = { clean() {}, writeLine: log, write: log };
    loader = new ESPLoader({ transport, baudrate: BAUD, romBaudrate: BAUD, terminal });
    const chip = await loader.main();
    if (!/ESP32(?!-)/.test(chip)) throw new Error(`This is an ${chip}, not the ESP32 in an LED Art Panel.`);
    const size = await loader.detectFlashSize();
    if (size === "8MB") throw new Error("This chip has 8 MB of flash, like a Tidbyt Gen 1, not the 4 MB of an LED Art Panel. Nothing was written. If it is a Tidbyt, choose Tidbyt above and back it up first: installing the art panel's image would erase Tidbyt's original firmware.");
    if (size !== want.flash) throw new Error(`This chip has ${size || "an unknown amount of"} flash; an LED Art Panel has 4 MB. Nothing was written.`);
    const rev = await loader.chip.getChipRevision(loader);
    if (rev < want.minRev) throw new Error(`This ESP32 is revision ${rev}; screeny needs revision ${want.minRev} or later. Nothing was written.`);
    artChecked = true;
    say(status, `Found an ESP32 (revision ${rev}) with 4 MB of flash: an LED Art Panel. Install is ready; pick the same port again.`, "ok");
  } catch (e) {
    say(status, friendly(e, "panel"), "bad");
  } finally {
    if (loader) {
      try { await loader.after("hard_reset"); } catch (e) { /* the port may already be gone */ }
    }
    if (transport) await transport.disconnect().catch(() => {});
    setBusy(false);
  }
}

async function restore() {
  const status = $("restore-status");
  const bar = $("restore-bar");
  const file = $("restore-file").files[0];
  if (!file) return;
  setBusy(true);
  let dev = null;
  try {
    const img = new Uint8Array(await file.arrayBuffer());
    if (img.length !== FLASH_BYTES) throw new Error(`${file.name} is ${img.length.toLocaleString()} bytes; a whole-flash backup is ${FLASH_BYTES.toLocaleString()}.`);
    if (!looksLikeFlash(img)) throw new Error(`${file.name} does not look like an ESP32 flash image.`);
    say(status, "Pick the Tidbyt's port (CP2102 or USB Serial)...");
    dev = await connect((s) => console.log("[restore]", s));
    bar.hidden = false;
    await dev.loader.writeFlash({
      fileArray: [{ data: img, address: 0 }],
      flashMode: "keep", flashFreq: "keep", flashSize: "keep",
      eraseAll: false, compress: true,
      reportProgress: (_i, written, total) => {
        bar.value = written / total;
        say(status, `Writing ${Math.round((100 * written) / total)}%. Keep the Tidbyt plugged in.`);
      },
      calculateMD5Hash: md5hex, // esptool-js checks the written flash against this
    });
    await release(dev);
    dev = null;
    say(status, `Restored ${file.name}. The Tidbyt is restarting with the firmware it had when you saved it.`, "ok");
  } catch (e) {
    say(status, friendly(e), "bad");
  } finally {
    if (dev) await release(dev);
    bar.hidden = true;
    setBusy(false);
  }
}

function friendly(e, who = "Tidbyt") {
  const msg = (e && e.message) || String(e);
  if (e && e.name === "NotFoundError") return `No port picked. Plug the ${who} in and try again.`;
  if (/Failed to open serial port|already open/i.test(msg)) return `The port is busy. Close other tabs or programs using it, unplug and replug the ${who}, and try again.`;
  if (/Failed to connect|Timeout|timed out/i.test(msg)) return `The ${who} did not answer. Unplug and replug the cable and try again. ` + msg;
  return msg;
}

function save(bytes, name) {
  const url = URL.createObjectURL(new Blob([bytes], { type: "application/octet-stream" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60000);
}

// The stub only offers MD5 for checking flash ranges, and WebCrypto has no MD5.
const S = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
const K = Array.from({ length: 64 }, (_, i) => Math.floor(Math.abs(Math.sin(i + 1)) * 2 ** 32) >>> 0);
function md5hex(bytes) {
  const n = bytes.length;
  const padded = new Uint8Array((((n + 8) >> 6) + 1) << 6);
  padded.set(bytes);
  padded[n] = 0x80;
  const view = new DataView(padded.buffer);
  view.setUint32(padded.length - 8, (n * 8) >>> 0, true);
  view.setUint32(padded.length - 4, Math.floor(n / 0x20000000), true);
  let a0 = 0x67452301, b0 = 0xefcdab89, c0 = 0x98badcfe, d0 = 0x10325476;
  const M = new Uint32Array(16);
  for (let off = 0; off < padded.length; off += 64) {
    for (let i = 0; i < 16; i++) M[i] = view.getUint32(off + i * 4, true);
    let a = a0, b = b0, c = c0, d = d0;
    for (let i = 0; i < 64; i++) {
      let f, g;
      if (i < 16) { f = (b & c) | (~b & d); g = i; }
      else if (i < 32) { f = (d & b) | (~d & c); g = (5 * i + 1) & 15; }
      else if (i < 48) { f = b ^ c ^ d; g = (3 * i + 5) & 15; }
      else { f = c ^ (b | ~d); g = (7 * i) & 15; }
      const s = S[(i >> 4) * 4 + (i & 3)];
      const x = (a + f + K[i] + M[g]) >>> 0;
      a = d; d = c; c = b;
      b = (b + ((x << s) | (x >>> (32 - s)))) >>> 0;
    }
    a0 = (a0 + a) >>> 0; b0 = (b0 + b) >>> 0; c0 = (c0 + c) >>> 0; d0 = (d0 + d) >>> 0;
  }
  let hex = "";
  for (const w of [a0, b0, c0, d0]) for (let i = 0; i < 4; i++) hex += ((w >>> (8 * i)) & 0xff).toString(16).padStart(2, "0");
  return hex;
}
export { md5hex };

// The board choice: shows that board's steps and points Install at its manifest. A
// change of board never carries the other board's backup or check across.
function choose(b) {
  if (!BOARDS[b]) b = "tidbyt";
  board = b;
  document.body.dataset.board = b;
  const r = document.querySelector(`input[name=board][value="${b}"]`);
  if (r) r.checked = true;
  $("install-button").setAttribute("manifest", BOARDS[b].manifest);
  showVersion();
  setBusy(false);
}

// Each manifest is generated at deploy time and names its firmware version. A release
// without an art panel image has no manifest-art-panel.json: the choice then goes.
const versions = {};
function showVersion() {
  const v = versions[board] || "";
  $("ver").textContent = v;
  $("ver2").textContent = versions["tidbyt"] || "-";
}
async function loadManifests() {
  for (const [b, { manifest }] of Object.entries(BOARDS)) {
    try {
      const r = await fetch(manifest);
      if (!r.ok) throw new Error(r.status);
      versions[b] = (await r.json()).version;
    } catch (e) {
      if (b !== "tidbyt") {
        $("board").hidden = true;
        if (board === b) choose("tidbyt");
      }
    }
  }
  showVersion();
}

if (typeof document !== "undefined" && $("backup-go")) {
  $("backup-go").addEventListener("click", backup);
  $("check-go").addEventListener("click", checkArtPanel);
  $("restore-go").addEventListener("click", restore);
  $("restore-file").addEventListener("change", () => setBusy(false));
  $("skip").addEventListener("change", () => setBusy(false));
  for (const r of document.querySelectorAll("input[name=board]")) r.addEventListener("change", () => choose(r.value));
  // ?board=art-panel opens the page with the art panel chosen.
  choose(new URLSearchParams(location.search).get("board") || "tidbyt");
  loadManifests();
}
