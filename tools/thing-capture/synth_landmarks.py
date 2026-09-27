#!/usr/bin/env python3
"""Synthesize a MediaPipe-shaped hand-landmark sequence, for testing
`convert.py` without a video, mediapipe, or the owner's own footage (none of
which exist yet - card 333's footage is expected 2026-09-27).

This is **not** derived from the Rust rig (`crates/art/src/patches/thing/
hand_rig.rs`): re-implementing that FK a second time in Python just to round
-trip it would be a second copy of the same geometry to keep in sync for no
real gain, since the point of this script is only to exercise
`convert.py`'s own pipeline (gap-filling, the One Euro filter, the per-frame
fit, contact detection, the output schema) against *some* independently-
generated, plausible motion - closer in spirit to the card's other stand-in
option, "a public sample hand video", than to a round trip. It is its own
small procedural hand: a palm that opens and closes while the wrist drifts
and dips towards a table, with per-frame Gaussian jitter and a few dropped-
confidence frames standing in for what a real capture would need smoothing
and gap-filling for.

Output: MediaPipe World Landmarks' own shape - 21 `(x, y, z)` points per
frame, metres, hand-scale - wrapped in the small JSON envelope `convert.py`
reads (`--landmarks-json`). See `README.md` for the field names.

Usage: `python3 synth_landmarks.py --out /tmp/synth.landmarks.json`
"""

import argparse
import json
import math
import random


def v_add(a, b):
    return (a[0] + b[0], a[1] + b[1], a[2] + b[2])


def v_scale(a, k):
    return (a[0] * k, a[1] * k, a[2] * k)


# Finger layout, matching `hand_rig.rs`'s own constants (a hand-scale of
# about 0.19 m) - duplicated here deliberately: this is a *test fixture*
# generator, not the rig itself, so it is allowed to know roughly what a
# hand looks like without being the one place that FK lives.
SPREAD_X = [-0.028, -0.008, 0.013, 0.032]
PALM_LEN = [0.080, 0.089, 0.086, 0.076]
PROX_LEN = [0.042, 0.048, 0.044, 0.032]
MID_LEN = [0.025, 0.029, 0.027, 0.019]
DIST_LEN = [0.017, 0.019, 0.017, 0.015]
THUMB_ANCHOR = (-0.042, 0.011, 0.019)
THUMB_META_LEN = 0.027
THUMB_PROX_LEN = 0.025
THUMB_DIST_LEN = 0.019


def finger_landmarks(wrist, right, up, fwd, spread_x, palm_len, lens, flex, pip_flex, dip_flex, spread_ang):
    anchor = v_add(wrist, v_add(v_scale(right, spread_x), v_scale(up, palm_len)))
    a2 = flex + spread_ang * 0.15  # a little of the spread leaks into flex-plane placement, plausibly
    d1 = v_add(v_scale(up, math.cos(a2)), v_scale(fwd, math.sin(a2)))
    d1 = v_add(d1, v_scale(right, math.sin(spread_ang) * 0.3))
    pip = v_add(anchor, v_scale(d1, lens[0]))
    a3 = flex + pip_flex
    d2 = v_add(v_scale(up, math.cos(a3)), v_scale(fwd, math.sin(a3)))
    dip = v_add(pip, v_scale(d2, lens[1]))
    a4 = a3 + dip_flex
    d3 = v_add(v_scale(up, math.cos(a4)), v_scale(fwd, math.sin(a4)))
    tip = v_add(dip, v_scale(d3, lens[2]))
    return [anchor, pip, dip, tip]


def synth(duration_s: float, fps: float, seed: int, drop_confidence_every: int):
    rng = random.Random(seed)
    frames = []
    n = int(duration_s * fps)
    for i in range(n):
        t = i / fps
        # A slow drift and a dip towards the table, plus a curl-open cycle.
        # A middling spot on the box's own walkable floor (`box_scene.rs`'s
        # `WALK_NEAR_Z`..`WALK_FAR_Z`, 0.30..3.48) - the box's camera sits
        # well *behind* its own front window (`Camera::at`'s `eye.z =
        # -1.55`), so `HAND_H` (not this position) is what makes the hand
        # read at panel scale; see `hand_rig.rs::HAND_H`'s own doc.
        wrist = (0.02 * math.sin(t * 0.6), 0.12 + 0.03 * math.sin(t * 0.9 + 1.0), 1.2 + 0.05 * math.cos(t * 0.5))
        right, up, fwd = (1.0, 0.0, 0.0), (0.0, 1.0, 0.0), (0.0, 0.0, 1.0)
        curl = 0.55 + 0.55 * math.sin(t * 1.3)  # 0 (open) .. ~1.1 (curled)

        landmarks = [None] * 21
        landmarks[0] = wrist
        thumb_anchor = v_add(wrist, v_add(v_scale(right, THUMB_ANCHOR[0]), v_add(v_scale(up, THUMB_ANCHOR[1]), v_scale(fwd, THUMB_ANCHOR[2]))))
        landmarks[1] = thumb_anchor
        t_flex, t_ab = 0.3 + 0.2 * math.sin(t * 1.1), 0.5
        td1 = v_add(v_scale(up, math.cos(t_flex)), v_scale(fwd, math.sin(t_flex)))
        td1 = v_add(td1, v_scale(right, -math.sin(t_ab) * 0.4))
        mcp = v_add(thumb_anchor, v_scale(td1, THUMB_META_LEN))
        landmarks[2] = mcp
        a2 = t_flex + curl * 0.4
        td2 = v_add(v_scale(up, math.cos(a2)), v_scale(fwd, math.sin(a2)))
        ip = v_add(mcp, v_scale(td2, THUMB_PROX_LEN))
        landmarks[3] = ip
        a3 = a2 + curl * 0.3
        td3 = v_add(v_scale(up, math.cos(a3)), v_scale(fwd, math.sin(a3)))
        landmarks[4] = v_add(ip, v_scale(td3, THUMB_DIST_LEN))

        for f in range(4):
            spread_ang = (f - 1.5) * 0.05
            pts = finger_landmarks(
                wrist, right, up, fwd, SPREAD_X[f], PALM_LEN[f], (PROX_LEN[f], MID_LEN[f], DIST_LEN[f]), curl * 0.5, curl * 0.6, curl * 0.5, spread_ang
            )
            landmarks[5 + f * 4 : 9 + f * 4] = pts

        # Per-coordinate jitter, the noise the One Euro filter earns its
        # keep against.
        noisy = [tuple(c + rng.gauss(0.0, 0.0025) for c in p) for p in landmarks]
        confidence = 1.0
        if drop_confidence_every and (i + 1) % drop_confidence_every == 0:
            confidence = 0.1  # a frame `convert.py` should treat as a gap
        frames.append({"t": t, "landmarks": noisy, "confidence": confidence})
    return {"fps": fps, "frames": frames}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out", required=True)
    ap.add_argument("--duration", type=float, default=2.0)
    ap.add_argument("--fps", type=float, default=30.0)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--drop-confidence-every", type=int, default=17, help="0 to disable")
    args = ap.parse_args()
    data = synth(args.duration, args.fps, args.seed, args.drop_confidence_every)
    with open(args.out, "w") as f:
        json.dump(data, f)
    print(f"wrote {len(data['frames'])} frames to {args.out}")


if __name__ == "__main__":
    main()
