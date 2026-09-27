#!/usr/bin/env python3
"""Video -> MediaPipe Hand Landmarker -> a `thing` clip.

Card 333: the Addams Family hand patch needs the owner's own captured hand
motion, converted into the clip format `crates/art/src/patches/thing/
clip.rs` loads (`ClipFrame`: `t`, `root_pos`, `root_rot` (a quaternion),
`pose` (matching `hand_rig::Pose` exactly), `contacts`). This is that
converter. See `README.md` in this directory for the owner's own steps;
this docstring is for whoever maintains the script.

Pipeline, per the card:

    video -> MediaPipe world landmarks (21 x, y, z per frame)
          -> gaps/low-confidence frames filled by linear interpolation
          -> a One Euro filter on every landmark coordinate (low-lag smoothing)
          -> a per-frame fit of the rig to the landmarks (this file's `fit_frame`)
          -> temporal regularisation of the *fitted* joint angles
          -> contact detection (a fingertip near the table and nearly still)
          -> a `.clip.json` file, plus an SVG preview

**Two ways in**, so this is testable without a video, MediaPipe, or the
owner's footage (none of which exist yet):

  --video PATH            MediaPipe Hand Landmarker on real frames. Needs
                          `mediapipe` and `opencv-python` (see
                          `requirements.txt`) - only this path does.
  --landmarks-json PATH   Skip straight to the landmark stage, reading the
                          small JSON envelope MediaPipe's own world
                          landmarks would have produced:
                          `{"fps": 30.0, "frames": [{"t": 0.0,
                          "landmarks": [[x,y,z], ...21], "confidence": 1.0},
                          ...]}`. `synth_landmarks.py` writes exactly this,
                          which is how this script is tested today (see the
                          README's "Testing without footage" section).

Everything below `--landmarks-json`/the MediaPipe call is the same code
either way, which is the point: nothing here is stubbed out for the test
path and swapped for a "real" one later.
"""

import argparse
import json
import math
import sys


# --------------------------------------------------------------- vectors


def v_add(a, b):
    return (a[0] + b[0], a[1] + b[1], a[2] + b[2])


def v_sub(a, b):
    return (a[0] - b[0], a[1] - b[1], a[2] - b[2])


def v_scale(a, k):
    return (a[0] * k, a[1] * k, a[2] * k)


def v_dot(a, b):
    return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]


def v_cross(a, b):
    return (a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0])


def v_len(a):
    return math.sqrt(v_dot(a, a))


def v_norm(a):
    n = v_len(a)
    return v_scale(a, 1.0 / n) if n > 1e-9 else (0.0, 1.0, 0.0)


# --------------------------------------------------------------- One Euro filter
#
# Casiez, Roussel & Vogel, 2012 - the standard low-lag filter for noisy,
# real-time-ish signals like hand landmarks: a low-pass filter whose own
# cutoff frequency rises with the signal's speed, so a still hand is smoothed
# hard and a fast one is smoothed lightly (and lags less for it).


class LowPass:
    def __init__(self):
        self.y = None

    def filter(self, x, alpha):
        if self.y is None:
            self.y = x
        else:
            self.y = alpha * x + (1.0 - alpha) * self.y
        return self.y


class OneEuroFilter:
    def __init__(self, min_cutoff=1.2, beta=0.02, d_cutoff=1.0):
        self.min_cutoff = min_cutoff
        self.beta = beta
        self.d_cutoff = d_cutoff
        self.x_filt = LowPass()
        self.dx_filt = LowPass()
        self.last_t = None

    @staticmethod
    def _alpha(rate, cutoff):
        tau = 1.0 / (2.0 * math.pi * cutoff)
        te = 1.0 / rate
        return 1.0 / (1.0 + tau / te)

    def filter(self, x, t):
        if self.last_t is None:
            rate = 30.0
        else:
            rate = 1.0 / max(t - self.last_t, 1e-6)
        self.last_t = t
        prev = self.x_filt.y if self.x_filt.y is not None else x
        dx = (x - prev) * rate
        edx = self.dx_filt.filter(dx, self._alpha(rate, self.d_cutoff))
        cutoff = self.min_cutoff + self.beta * abs(edx)
        return self.x_filt.filter(x, self._alpha(rate, cutoff))


# --------------------------------------------------------------- gaps


def fill_gaps(frames, confidence_threshold):
    """Linear-interpolate any frame below `confidence_threshold` (or with no
    landmarks at all) between its nearest good neighbours. Returns the
    number of frames that were interpolated, for the report the card asks
    for ("gaps... interpolated and reported")."""
    n = len(frames)
    good = [f.get("landmarks") is not None and f.get("confidence", 1.0) >= confidence_threshold for f in frames]
    if not any(good):
        raise SystemExit("every frame is low-confidence or missing landmarks - nothing to convert")
    filled = 0
    i = 0
    while i < n:
        if good[i]:
            i += 1
            continue
        j = i
        while j < n and not good[j]:
            j += 1
        lo = i - 1
        hi = j if j < n else lo
        for k in range(i, j):
            filled += 1
            if lo < 0:
                frames[k]["landmarks"] = frames[hi]["landmarks"]
            elif hi >= n or hi == lo:
                frames[k]["landmarks"] = frames[lo]["landmarks"]
            else:
                u = (k - lo) / (hi - lo)
                frames[k]["landmarks"] = [
                    tuple(a + (b - a) * u for a, b in zip(frames[lo]["landmarks"][p], frames[hi]["landmarks"][p])) for p in range(21)
                ]
        i = j
    return filled


# --------------------------------------------------------------- smoothing


def smooth_landmarks(frames):
    filters = [[OneEuroFilter() for _ in range(3)] for _ in range(21)]
    for fr in frames:
        t = fr["t"]
        fr["landmarks"] = [tuple(filters[p][c].filter(fr["landmarks"][p][c], t) for c in range(3)) for p in range(21)]


# --------------------------------------------------------------- joint limits
#
# Duplicated from `hand_rig.rs::limits` on purpose (a converter script and
# the rig it feeds are allowed to know the same numbers without sharing a
# file across a Python/Rust boundary) - see this repo's README on why
# `crates/proto` is the one place that rule is strict (wire format), which
# this is not.

LIMITS = {
    "wrist_yaw": (-0.35, 0.35),
    "wrist_pitch": (-1.2, 0.9),
    "wrist_roll": (-0.5, 0.5),
    "spread": (-0.35, 0.35),
    "mcp": (-0.15, 1.6),
    "pip": (0.0, 1.7),
    "dip": (0.0, 1.5),
    "cmc_flex": (-0.3, 1.0),
    "cmc_abduct": (-0.2, 1.2),
    "thumb_mcp": (0.0, 1.2),
    "thumb_ip": (-0.2, 1.4),
}


def clamp(x, lo, hi):
    return max(lo, min(hi, x))


# --------------------------------------------------------------- basis -> quaternion


def basis_to_quat(right, up, fwd):
    """A right-handed orthonormal basis to a unit quaternion `(x, y, z, w)`,
    matching `geom::Quat`'s own convention (checked directly against it in
    `test_basis_to_quat_matches_rust_convention.py` in this directory).
    Shepperd's method (numerically stable for any rotation, not just small
    ones)."""
    m00, m01, m02 = right[0], up[0], fwd[0]
    m10, m11, m12 = right[1], up[1], fwd[1]
    m20, m21, m22 = right[2], up[2], fwd[2]
    trace = m00 + m11 + m22
    if trace > 0:
        s = math.sqrt(trace + 1.0) * 2.0
        w = 0.25 * s
        x = (m21 - m12) / s
        y = (m02 - m20) / s
        z = (m10 - m01) / s
    elif m00 > m11 and m00 > m22:
        s = math.sqrt(1.0 + m00 - m11 - m22) * 2.0
        w = (m21 - m12) / s
        x = 0.25 * s
        y = (m01 + m10) / s
        z = (m02 + m20) / s
    elif m11 > m22:
        s = math.sqrt(1.0 + m11 - m00 - m22) * 2.0
        w = (m02 - m20) / s
        x = (m01 + m10) / s
        y = 0.25 * s
        z = (m12 + m21) / s
    else:
        s = math.sqrt(1.0 + m22 - m00 - m11) * 2.0
        w = (m10 - m01) / s
        x = (m02 + m20) / s
        y = (m12 + m21) / s
        z = 0.25 * s
    return (x, y, z, w)


# --------------------------------------------------------------- the fit

FINGER_NAMES = ["index", "middle", "ring", "pinky"]


def bone_angle_in_basis(bone, up, fwd):
    """This bone's flex angle in the (up, fwd) plane - the same convention
    `hand_rig.rs`'s FK uses (increasing angle rotates `up` towards `fwd`),
    so a fit built this way plugs straight into `Pose::from_array`."""
    return math.atan2(v_dot(bone, fwd), v_dot(bone, up))


def fit_frame(landmarks):
    """One frame of landmarks -> `(root_pos, root_rot, pose_dict, tips)`.

    The root basis comes from the palm's own three landmarks (wrist, index
    MCP, pinky MCP): `up` towards the middle finger, `fwd` the palm's own
    normal, `right` completing the frame - and every finger's flex is then
    read off in *that* frame, which is `hand_rig.rs`'s own approximation
    (both hinges below a joint share one axis) turned around into a fit
    rather than a render.
    """
    wrist = landmarks[0]
    index_mcp, middle_mcp, ring_mcp, pinky_mcp = landmarks[5], landmarks[9], landmarks[13], landmarks[17]
    up = v_norm(v_sub(middle_mcp, wrist))
    # `right` runs thumb-side to pinky-side across the palm - `pinky - index`
    # points that way directly (matching `hand_rig.rs`'s own `SPREAD_X`
    # convention, increasing away from the thumb), which is more direct and
    # less sign-fragile than building it by crossing two wrist-relative
    # vectors first (an earlier draft did that and got the palm normal
    # backwards - see `test_fit_frame_basis_matches_a_known_pose` below,
    # which pins the fix).
    right_guess = v_norm(v_sub(pinky_mcp, index_mcp))
    fwd = v_norm(v_cross(right_guess, up))
    right = v_cross(up, fwd)
    root_rot = basis_to_quat(right, up, fwd)

    fingers = []
    for i, mcp_idx in enumerate((5, 9, 13, 17)):
        mcp, pip, dip, tip = landmarks[mcp_idx : mcp_idx + 4]
        anchor = mcp  # the metacarpal is a fixed anchor in `hand_rig.rs`, not a fitted joint
        b1 = v_sub(pip, anchor)
        b2 = v_sub(dip, pip)
        b3 = v_sub(tip, dip)
        a1 = bone_angle_in_basis(b1, up, fwd)
        a2 = bone_angle_in_basis(b2, up, fwd)
        a3 = bone_angle_in_basis(b3, up, fwd)
        spread = math.atan2(v_dot(b1, right), math.hypot(v_dot(b1, up), v_dot(b1, fwd)))
        fingers.append(
            {
                "spread": clamp(spread, *LIMITS["spread"]),
                "mcp": clamp(a1, *LIMITS["mcp"]),
                "pip": clamp(a2 - a1, *LIMITS["pip"]),
                "dip": clamp(a3 - a2, *LIMITS["dip"]),
            }
        )

    cmc, tmcp, tip_, ttip = landmarks[1], landmarks[2], landmarks[3], landmarks[4]
    tb1, tb2, tb3 = v_sub(tmcp, cmc), v_sub(tip_, tmcp), v_sub(ttip, tip_)
    ta1 = bone_angle_in_basis(tb1, up, fwd)
    ta2 = bone_angle_in_basis(tb2, up, fwd)
    ta3 = bone_angle_in_basis(tb3, up, fwd)
    t_ab = math.atan2(v_dot(tb1, right), math.hypot(v_dot(tb1, up), v_dot(tb1, fwd)))
    thumb = {
        "cmc_flex": clamp(ta1, *LIMITS["cmc_flex"]),
        "cmc_abduct": clamp(t_ab, *LIMITS["cmc_abduct"]),
        "mcp": clamp(ta2 - ta1, *LIMITS["thumb_mcp"]),
        "ip": clamp(ta3 - ta2, *LIMITS["thumb_ip"]),
    }

    # `pose.wrist` (the *local* wrist bend on top of the root) is left at
    # zero: the whole measured orientation already went into `root_rot`
    # above, and the player only ever needs one of the two to carry it -
    # `hand_rig.rs` keeps both because a hand-authored placeholder finds it
    # natural to separate "where the whole hand is facing" from "how the
    # wrist is bent", but a fit from real landmarks has no such split to
    # make.
    pose = {"wrist": {"yaw": 0.0, "pitch": 0.0, "roll": 0.0}, "thumb": thumb, "fingers": fingers}
    tips = [landmarks[4], landmarks[8], landmarks[12], landmarks[16], landmarks[20]]
    return wrist, root_rot, pose, tips


POSE_FIELDS = (
    [("wrist", "yaw"), ("wrist", "pitch"), ("wrist", "roll"), ("thumb", "cmc_flex"), ("thumb", "cmc_abduct"), ("thumb", "mcp"), ("thumb", "ip")]
    + [(f"finger{i}", k) for i in range(4) for k in ("spread", "mcp", "pip", "dip")]
)


def pose_to_flat(pose):
    out = []
    for a, b in POSE_FIELDS:
        if a == "wrist":
            out.append(pose["wrist"][b])
        elif a == "thumb":
            out.append(pose["thumb"][b])
        else:
            out.append(pose["fingers"][int(a[-1])][b])
    return out


def flat_to_pose(flat):
    pose = {"wrist": {}, "thumb": {}, "fingers": [{} for _ in range(4)]}
    for (a, b), v in zip(POSE_FIELDS, flat):
        if a == "wrist":
            pose["wrist"][b] = v
        elif a == "thumb":
            pose["thumb"][b] = v
        else:
            pose["fingers"][int(a[-1])][b] = v
    return pose


def regularise(poses, alpha=0.35):
    """Temporal regularisation of the *fitted* joint angles (the card's own
    phrase), a second smoothing pass distinct from the One Euro filter on
    the raw landmarks: an exponential low-pass over each of the 23 fitted
    channels, catching the residual per-frame fit jitter (`atan2` is not
    perfectly smooth near its own wrap) that filtering the landmarks first
    does not fully remove."""
    if not poses:
        return poses
    flat = [pose_to_flat(p) for p in poses]
    out = [flat[0][:]]
    for i in range(1, len(flat)):
        prev = out[-1]
        out.append([alpha * flat[i][k] + (1.0 - alpha) * prev[k] for k in range(len(prev))])
    return [flat_to_pose(f) for f in out]


# --------------------------------------------------------------- contacts


def detect_contacts(tip_series, height_thresh=0.02, speed_thresh=0.08, fps=30.0):
    """A fingertip is "planted" when it is close to the table plane (the
    lowest any fingertip gets across the whole clip, a robust per-clip floor
    estimate) and nearly still. `[thumb, index, middle, ring, pinky]` per
    frame, matching `hand_rig::Joints::tips`."""
    n = len(tip_series)
    floor_y = min(p[1] for frame in tip_series for p in frame)
    contacts = []
    for i in range(n):
        row = []
        for f in range(5):
            y = tip_series[i][f][1]
            if i == 0 or i == n - 1:
                speed = 0.0
            else:
                speed = abs(tip_series[i + 1][f][1] - tip_series[i - 1][f][1]) * fps / 2.0
            row.append((y - floor_y) < height_thresh and speed < speed_thresh)
        contacts.append(row)
    return contacts


# --------------------------------------------------------------- fusion


def fuse_second_angle(frames_a, frames_b):
    """A second camera's landmarks, fused by averaging depth (`z`) frame by
    frame - a stated simplification, not full multi-view triangulation (the
    card allows this: "optional second camera angle fused if the owner
    provides one"). Assumes both sequences already share timestamps and
    frame count, which `--landmarks-json-b` requires and this checks."""
    if len(frames_a) != len(frames_b):
        raise SystemExit(f"angle A has {len(frames_a)} frames but angle B has {len(frames_b)} - they must match to fuse")
    for fa, fb in zip(frames_a, frames_b):
        fa["landmarks"] = [(pa[0], pa[1], (pa[2] + pb[2]) / 2.0) for pa, pb in zip(fa["landmarks"], fb["landmarks"])]
    return frames_a


# --------------------------------------------------------------- MediaPipe (real footage)


def landmarks_from_video(video_path, model_path, fps_out=30.0):
    try:
        import cv2
        import mediapipe as mp
    except ImportError as e:
        raise SystemExit(
            "`--video` needs `mediapipe` and `opencv-python` (see requirements.txt). "
            f"Import failed: {e}\nUse `--landmarks-json` to test the rest of the pipeline without them."
        )
    if not model_path:
        raise SystemExit(
            "`--video` needs `--model path/to/hand_landmarker.task` - see this "
            "directory's README ('Getting the model') for where to download it."
        )
    base = mp.tasks.BaseOptions
    hand_landmarker = mp.tasks.vision.HandLandmarker
    options = mp.tasks.vision.HandLandmarkerOptions(
        base_options=base(model_asset_path=model_path), num_hands=1, running_mode=mp.tasks.vision.RunningMode.VIDEO
    )
    cap = cv2.VideoCapture(video_path)
    frames = []
    with hand_landmarker.create_from_options(options) as landmarker:
        i = 0
        while True:
            ok, img = cap.read()
            if not ok:
                break
            t_ms = int(i * 1000.0 / fps_out)
            mp_image = mp.Image(image_format=mp.ImageFormat.SRGB, data=img)
            result = landmarker.detect_for_video(mp_image, t_ms)
            if result.hand_world_landmarks:
                lm = result.hand_world_landmarks[0]
                pts = [(p.x, p.y, p.z) for p in lm]
                frames.append({"t": i / fps_out, "landmarks": pts, "confidence": 1.0})
            else:
                frames.append({"t": i / fps_out, "landmarks": None, "confidence": 0.0})
            i += 1
    cap.release()
    return {"fps": fps_out, "frames": frames}


# --------------------------------------------------------------- SVG preview


def write_preview_svg(path, tip_series, contacts, wrists, n_cols=8):
    n = len(tip_series)
    step = max(1, n // n_cols)
    cell = 90
    cols = min(n_cols, (n + step - 1) // step)
    parts = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{cols * cell}" height="{cell}" style="background:#111">']
    for c, i in enumerate(range(0, n, step)):
        ox = c * cell + cell / 2
        oy = cell / 2
        scale = 220.0
        wx, wy = wrists[i][0] * scale + ox, -wrists[i][1] * scale + oy + 20
        for f in range(5):
            tx, ty = tip_series[i][f][0] * scale + ox, -tip_series[i][f][1] * scale + oy + 20
            colour = "#ffcc33" if contacts[i][f] else "#88ccff"
            parts.append(f'<line x1="{wx:.1f}" y1="{wy:.1f}" x2="{tx:.1f}" y2="{ty:.1f}" stroke="{colour}" stroke-width="1.5"/>')
            parts.append(f'<circle cx="{tx:.1f}" cy="{ty:.1f}" r="2" fill="{colour}"/>')
        parts.append(f'<circle cx="{wx:.1f}" cy="{wy:.1f}" r="3" fill="#ffffff"/>')
        parts.append(f'<text x="{c * cell + 4}" y="{cell - 4}" fill="#666" font-size="9">{i}</text>')
    parts.append("</svg>")
    with open(path, "w") as fh:
        fh.write("\n".join(parts))


def round_floats(obj, places=6):
    if isinstance(obj, float):
        return round(obj, places)
    if isinstance(obj, dict):
        return {k: round_floats(v, places) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [round_floats(v, places) for v in obj]
    return obj


# --------------------------------------------------------------- main


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--video", help="a phone clip; needs mediapipe + opencv-python")
    ap.add_argument("--model", help="path to hand_landmarker.task (see README's 'Getting the model'); required with --video")
    ap.add_argument("--landmarks-json", help="skip the video/MediaPipe step (see this file's docstring)")
    ap.add_argument("--landmarks-json-b", help="a second camera angle's landmarks, same shape, fused by averaged depth")
    ap.add_argument("--name", required=True, help="the clip's name, e.g. `wave` - matches the shot list")
    ap.add_argument("--loop", action="store_true", help="mark the clip loopable (a walk, `wave`) rather than one-shot")
    ap.add_argument("--out", required=True, help="e.g. crates/art/assets/thing/wave.clip.json")
    ap.add_argument("--preview", help="an SVG contact sheet path")
    ap.add_argument("--confidence-threshold", type=float, default=0.5)
    args = ap.parse_args()

    if bool(args.video) == bool(args.landmarks_json):
        raise SystemExit("give exactly one of --video or --landmarks-json")

    data = landmarks_from_video(args.video, args.model) if args.video else json.load(open(args.landmarks_json))
    frames = data["frames"]
    fps = data["fps"]

    if args.landmarks_json_b:
        frames = fuse_second_angle(frames, json.load(open(args.landmarks_json_b))["frames"])

    gaps = fill_gaps(frames, args.confidence_threshold)
    smooth_landmarks(frames)

    fitted = [fit_frame(fr["landmarks"]) for fr in frames]
    wrists = [f[0] for f in fitted]
    rots = [f[1] for f in fitted]
    poses = regularise([f[2] for f in fitted])
    tip_series = [f[3] for f in fitted]
    contacts = detect_contacts(tip_series, fps=fps)

    out_frames = []
    for i, fr in enumerate(frames):
        out_frames.append(
            {
                "t": fr["t"],
                "root_pos": list(wrists[i]),
                "root_rot": list(rots[i]),
                "pose": poses[i],
                "contacts": contacts[i],
            }
        )
    clip = {"name": args.name, "loopable": bool(args.loop), "frames": out_frames}
    with open(args.out, "w") as fh:
        # Compact (the card: "compact data files") and rounded - six
        # decimal places is well past what a 64x32 panel can ever show, and
        # cuts a typical clip's file size by more than half over `json.dump`
        # with its default float repr.
        json.dump(round_floats(clip), fh, separators=(",", ":"))

    print(f"{args.name}: {len(out_frames)} frames, {gaps} interpolated (gap/low-confidence), wrote {args.out}")
    if args.preview:
        write_preview_svg(args.preview, tip_series, contacts, wrists)
        print(f"preview: {args.preview}")


if __name__ == "__main__":
    main()
