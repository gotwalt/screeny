#!/usr/bin/env python3
"""Plain-assert tests for `convert.py`, runnable with nothing but the
standard library (`python3 test_convert.py`) - no pytest, no numpy: this
tool's own README asks for pinned versions for the *capture* dependencies
(mediapipe, opencv), and it would be a shame to then need a test dependency
just to check the parts that need neither."""

import math
import sys

import convert


def close(a, b, tol=1e-3):
    return abs(a - b) < tol


def test_fit_frame_basis_matches_a_known_pose():
    """A hand held flat, facing the camera, fingers pointing straight up
    the way `synth_landmarks.py` places it at rest (no curl): `fit_frame`
    should read back a basis matching world `right=(1,0,0)`, `up=(0,1,0)`,
    `fwd=(0,0,1)` - this is the regression for the sign bug an earlier
    draft had (`fwd` and `right` both came out negated, which put every
    fitted `mcp` hard against its own joint limit; see `convert.py`'s own
    comment on this in `fit_frame`)."""
    wrist = (0.0, 0.0, 0.0)
    landmarks = [wrist] + [(0.0, 0.0, 0.0)] * 20
    # index (5), middle (9), ring (13), pinky (17) MCPs, straight up with a
    # small right-ward spread increasing index -> pinky (the `SPREAD_X`
    # convention).
    for base, sx in ((5, -0.02), (9, -0.005), (13, 0.01), (17, 0.02)):
        landmarks[base] = (sx, 0.09, 0.0)
        landmarks[base + 1] = (sx, 0.13, 0.0)
        landmarks[base + 2] = (sx, 0.16, 0.0)
        landmarks[base + 3] = (sx, 0.18, 0.0)
    landmarks[1] = (-0.04, 0.01, 0.02)
    landmarks[2] = (-0.06, 0.03, 0.02)
    landmarks[3] = (-0.07, 0.05, 0.02)
    landmarks[4] = (-0.08, 0.07, 0.02)

    _, root_rot, pose, _ = convert.fit_frame(landmarks)
    x, y, z, w = root_rot
    # The identity rotation: `w` near 1, everything else near 0.
    assert close(w, 1.0, 0.05) and close(x, 0.0, 0.05) and close(y, 0.0, 0.05) and close(z, 0.0, 0.05), root_rot
    # Every finger dead straight: `mcp` (and `pip`, `dip`) near zero, not
    # slammed against `LIMITS["mcp"][0] == -0.15`.
    for f in pose["fingers"]:
        assert abs(f["mcp"]) < 0.1, pose["fingers"]
        assert abs(f["pip"]) < 0.05 and abs(f["dip"]) < 0.05, pose["fingers"]


def test_fill_gaps_interpolates_and_reports_the_count():
    good = (0.0, 0.0, 0.0)
    frames = [
        {"t": 0.0, "landmarks": [good] * 21, "confidence": 1.0},
        {"t": 1.0 / 30, "landmarks": None, "confidence": 0.0},
        {"t": 2.0 / 30, "landmarks": None, "confidence": 0.0},
        {"t": 3.0 / 30, "landmarks": [(0.03, 0.0, 0.0)] * 21, "confidence": 1.0},
    ]
    n = convert.fill_gaps(frames, 0.5)
    assert n == 2, n
    assert close(frames[1]["landmarks"][0][0], 0.01)
    assert close(frames[2]["landmarks"][0][0], 0.02)


def test_one_euro_filter_smooths_noise_without_much_lag():
    f = convert.OneEuroFilter()
    import random

    rng = random.Random(0)
    out = []
    for i in range(120):
        t = i / 30.0
        true = math.sin(t * 2.0) * 0.05
        out.append(f.filter(true + rng.gauss(0, 0.01), t))
    # The filtered signal's own noise (high-frequency wiggle against a
    # smooth local trend) should be well below the 0.01 the raw signal had.
    resid = [out[i] - (out[i - 1] + out[i + 1]) / 2.0 for i in range(1, len(out) - 1)]
    rms = math.sqrt(sum(r * r for r in resid) / len(resid))
    assert rms < 0.006, rms


def test_detect_contacts_flags_the_still_low_fingertip():
    # Four frames, index fingertip near the floor and still; everything
    # else well above it or moving.
    tips = []
    for i in range(4):
        tips.append([(0.0, 0.5, 0.0), (0.0, 0.001, 0.0), (0.0, 0.3, 0.0), (0.0, 0.3 + i * 0.05, 0.0), (0.0, 0.3, 0.0)])
    contacts = convert.detect_contacts(tips, fps=30.0)
    assert contacts[1][1] is True, contacts
    assert contacts[1][3] is False, "the moving fingertip should not be flagged planted"


def test_pose_flat_round_trips():
    pose = {
        "wrist": {"yaw": 0.1, "pitch": 0.2, "roll": 0.3},
        "thumb": {"cmc_flex": 0.4, "cmc_abduct": 0.5, "mcp": 0.6, "ip": 0.7},
        "fingers": [{"spread": i * 0.1, "mcp": i * 0.2, "pip": i * 0.3, "dip": i * 0.4} for i in range(4)],
    }
    flat = convert.pose_to_flat(pose)
    assert len(flat) == 23, len(flat)
    back = convert.flat_to_pose(flat)
    assert back == pose, back


def test_basis_to_quat_is_a_unit_quaternion_for_an_orthonormal_basis():
    import random

    rng = random.Random(2)
    for _ in range(20):
        # A random rotation, built the same way `fit_frame` builds one.
        ax = (rng.uniform(-1, 1), rng.uniform(-1, 1), rng.uniform(-1, 1))
        up = convert.v_norm(ax)
        arbitrary = (1.0, 0.0, 0.0) if abs(up[0]) < 0.9 else (0.0, 1.0, 0.0)
        right = convert.v_norm(convert.v_cross(arbitrary, up))
        fwd = convert.v_cross(right, up)
        x, y, z, w = convert.basis_to_quat(right, up, fwd)
        assert close(x * x + y * y + z * z + w * w, 1.0, 1e-3)


def main():
    tests = [v for k, v in list(globals().items()) if k.startswith("test_")]
    failed = 0
    for t in tests:
        try:
            t()
            print(f"ok   {t.__name__}")
        except AssertionError as e:
            failed += 1
            print(f"FAIL {t.__name__}: {e}")
    if failed:
        print(f"{failed}/{len(tests)} failed")
        sys.exit(1)
    print(f"{len(tests)}/{len(tests)} passed")


if __name__ == "__main__":
    main()
