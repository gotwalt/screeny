# thing-capture

Turns the owner's own phone footage of his hand (card 333's shot list,
`.claude/process/board/doing/333-thing-shot-list.md`) into the clip files
the `thing` patch plays (`crates/art/src/patches/thing/clip.rs`). Offline,
Python, not shipped - the same kind of tool `tools/art-faces.py` already is
for this repo.

## The owner's steps

1. Shoot the shot list. Copy the files into `~/src/screeny/captures/thing/`
   (gitignored - the videos never get committed, only what this tool
   extracts from them).
2. Install this directory's own dependencies once:

   ```sh
   cd tools/thing-capture
   pip install -r requirements.txt
   ```

3. Get the Hand Landmarker model (a one-time download, not part of this
   repo - see "Getting the model" below).
4. Convert each take:

   ```sh
   python3 convert.py \
     --video ~/src/screeny/captures/thing/wave.mp4 \
     --model ~/hand_landmarker.task \
     --name wave \
     --loop \
     --out ../../crates/art/assets/thing/wave.clip.json \
     --preview /tmp/wave.svg
   ```

   `--name` is the clip's name and must match the shot list's own names
   (`walk-scuttle`, `wave`, `point`, ...) - that is what the choreographer
   expects (`.claude/process/board/doing/333-thing-shot-list.md`'s own
   line: "The clip names there are the names the choreographer should
   expect"). Pass `--loop` for anything meant to repeat (a walk, `wave`,
   `look-around`, `drum`); leave it off for a one-shot gesture (`point`,
   `thumbs-up`, `snap`, ...) - the choreographer runs a loopable clip for a
   few seconds and a one-shot exactly once.

   If a take has a second angle (`wave-B.mp4`), add
   `--landmarks-json-b` pointing at its own converted landmarks (run this
   script once per angle up to the landmark stage, or extend it - see
   "Fusing a second angle" below) so the depth estimate improves.

5. Open `/tmp/wave.svg` in a browser: a row of stick-figure fingertips, one
   per few frames, orange where a fingertip is flagged planted. This is the
   "did the fit come out sane" check the card asks for; a proper look at
   the actual render is `screeny-art snapshot thing --at N --out x.png`
   once the file is in place (step 6).
6. Rebuild. `crates/art/src/patches/thing/clip.rs`'s `ALL_CLIPS` is the one
   place a new file needs a line added (mirroring `patches/mod.rs`'s own
   `ALL` list) - add

   ```rust
   ("wave", include_str!("../../../assets/thing/wave.clip.json")),
   ```

   next to the others, and rebuild (`cargo build -p screeny-art`). From
   then on `wave` is a real clip, not a placeholder - `Patch::playing()`
   stops labelling it "(placeholder motion)" automatically
   (`clip::is_placeholder`), no other code to touch.

Repeat for every take. `converted-demo.clip.json` (already in
`crates/art/assets/thing/` and already wired into `ALL_CLIPS`) is what this
whole pipeline produces run against a synthetic stand-in - see "Testing
without footage" below - and is there so the loading path has something
real to prove itself against before the owner's own footage exists.

## Getting the model

MediaPipe's Hand Landmarker needs a `.task` model file, which is not part of
this repository (a few megabytes, and not something to keep in a git repo
that is about to go public with no large binaries in it). Download the
"float16" hand landmarker task file from Google's MediaPipe model page
(search "MediaPipe Hand Landmarker model") and pass its path as `--model`.
Any location works; nothing here assumes one.

## Testing without footage

The owner's footage does not exist yet (expected 2026-09-27). Everything
past the landmark stage is tested against a synthetic, independently
written stand-in instead of waiting for it:

```sh
python3 synth_landmarks.py --out /tmp/synth.landmarks.json
python3 convert.py --landmarks-json /tmp/synth.landmarks.json \
  --name converted-demo --out /tmp/out.clip.json --preview /tmp/out.svg
python3 test_convert.py
```

`synth_landmarks.py`'s own docstring explains why it is a second,
independently-written procedural hand rather than a round trip through
`hand_rig.rs`'s own FK (re-implementing that a second time in Python to
prove a Python-to-Rust round trip was judged not worth the duplication for
what this needed to prove). `test_convert.py` is plain `assert`, stdlib
only - `python3 test_convert.py`, no pytest, no numpy - and is what caught
a real sign bug in the palm-basis fit before it ever reached a real clip
(see the card's Log for 2026-09-26).

## Fusing a second angle

`--landmarks-json-b` is the same JSON shape as `--landmarks-json`, and
`convert.py` averages the two sequences' own `z` (depth) per landmark per
frame - a stated simplification (the card allows this: "optional second
camera angle fused if the owner provides one"), not full multi-view
triangulation, and it requires both sequences to already share the same
frame count and timestamps. `--video` does not yet have an equivalent
`--video-b`; run the MediaPipe stage once per angle (two separate
`--video` calls with `--landmarks-json`-only output, by reading this
script's `landmarks_from_video` directly, or wait for a follow-up card) and
fuse at the landmark-JSON stage instead.

## What is real and what is a stand-in

- The **filter, the fit, contact detection, the output schema, and the
  Rust loading path** are real and tested, run against real (if
  synthetic) landmark data - `converted-demo.clip.json` is genuine output,
  not a fixture typed by hand.
- The **rig fit** is a direct geometric read of joint angles from landmark
  positions (palm basis -> per-finger flex/spread by simple trigonometry),
  not a full inverse-kinematics optimisation against bone-length and
  joint-limit constraints simultaneously - it clamps to the same limits
  `hand_rig.rs::limits` uses, but does not *solve* for the closest pose
  that respects them the way a proper IK solver would when the raw
  landmarks disagree with a rigid bone length. Good enough for a first
  pass; a follow-up if the owner's real footage shows it matters.
- The **wrist's own local bend** (`pose.wrist` in the clip schema) is
  always written as zero by this converter - the whole measured
  orientation goes into `root_rot` instead (see `convert.py::fit_frame`'s
  comment on this). `hand_rig.rs` keeps the two separate because a
  hand-authored placeholder finds that split natural; a fit from real
  landmarks does not need it.
