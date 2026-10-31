//! The hand behind the veil: a real skinned model (`hand.glb`, the WebXR
//! generic right hand, MIT - see `hand.LICENSE`), posed by bending its
//! fingers - by hand, or as a captured take (`clips.rs`) bent them - then
//! laid against the sheet.
//!
//! The model's joints are all children of one root and carry their rest
//! transforms directly, so a pose is built here rather than read: each
//! finger is a chain of rotations about its knuckles, and a vertex moves with
//! the weighted blend of its joints' rotations. In the rest pose the fingers
//! point down -Y, fan out along Z, and the palm faces -X.

use serde_json::Value;
use std::sync::OnceLock;

type V3 = [f32; 3];

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(a: V3, k: f32) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn unit(a: V3) -> V3 {
    scale(a, 1.0 / dot(a, a).sqrt().max(1e-9))
}

/// A rigid motion: rotate by `r` (rows), then move by `t`.
#[derive(Clone, Copy, Debug)]
pub struct Rigid {
    r: [V3; 3],
    t: V3,
}

impl Rigid {
    pub const ID: Rigid = Rigid { r: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], t: [0.0; 3] };

    pub fn apply(&self, p: V3) -> V3 {
        add([dot(self.r[0], p), dot(self.r[1], p), dot(self.r[2], p)], self.t)
    }

    fn turn(&self, p: V3) -> V3 {
        [dot(self.r[0], p), dot(self.r[1], p), dot(self.r[2], p)]
    }

    /// `self` after `other`.
    fn then_after(&self, other: &Rigid) -> Rigid {
        let col = |m: &Rigid, j: usize| [m.r[0][j], m.r[1][j], m.r[2][j]];
        let mut r = [[0.0; 3]; 3];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = dot(self.r[i], col(other, j));
            }
        }
        Rigid { r, t: self.apply(other.t) }
    }

    /// Rotation by `angle` about `axis` (unit) through `pivot`.
    fn about(pivot: V3, axis: V3, angle: f32) -> Rigid {
        let (s, c) = angle.sin_cos();
        let [x, y, z] = axis;
        let k = 1.0 - c;
        let r = [
            [c + x * x * k, x * y * k - z * s, x * z * k + y * s],
            [y * x * k + z * s, c + y * y * k, y * z * k - x * s],
            [z * x * k - y * s, z * y * k + x * s, c + z * z * k],
        ];
        let m = Rigid { r, t: [0.0; 3] };
        Rigid { r, t: sub(pivot, m.turn(pivot)) }
    }

    /// The rotation taking the frame (a, b) onto (a2, b2), each pair made
    /// orthonormal with `a` kept exactly.
    pub fn between(a: V3, b: V3, a2: V3, b2: V3) -> Rigid {
        let frame = |a: V3, b: V3| {
            let a = unit(a);
            let b = unit(sub(b, scale(a, dot(a, b))));
            [a, b, cross(a, b)]
        };
        let (f, g) = (frame(a, b), frame(a2, b2));
        let mut r = [[0.0; 3]; 3];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = (0..3).map(|k| g[k][i] * f[k][j]).sum();
            }
        }
        Rigid { r, t: [0.0; 3] }
    }

    pub fn moved(mut self, by: V3) -> Rigid {
        self.t = add(self.t, by);
        self
    }

    /// The rotation undone.
    fn inverse_turn(&self) -> Rigid {
        let r = std::array::from_fn(|i| std::array::from_fn(|j| self.r[j][i]));
        Rigid { r, t: [0.0; 3] }
    }

    /// The shortest rotation taking direction `a` onto `b` (both unit).
    fn swing(a: V3, b: V3) -> Rigid {
        let axis = cross(a, b);
        let s = dot(axis, axis).sqrt();
        if s < 1e-6 {
            return Rigid::ID;
        }
        Rigid::about([0.0; 3], scale(axis, 1.0 / s), s.atan2(dot(a, b)))
    }

    /// This rotation, about `pivot` instead of the origin.
    fn pivoted(&self, pivot: V3) -> Rigid {
        Rigid { r: self.r, t: sub(pivot, self.turn(pivot)) }
    }

    pub fn scaled(mut self, k: f32) -> Rigid {
        for row in &mut self.r {
            *row = scale(*row, k);
        }
        self.t = scale(self.t, k);
        self
    }
}

/// How the hand is held: how far each digit curls (thumb first, 0 straight,
/// 1 a fist) and how wide the fingers spread (radians between neighbours).
/// With `captured`, each knuckle first bends as a take bent it (see
/// `Model::captured`), and the curl is a bend on top of that.
#[derive(Clone, Copy, Debug)]
pub struct Pose {
    pub curl: [f32; 5],
    pub spread: f32,
    pub captured: Option<Captured>,
}

/// Each knuckle's turn, digit by digit from the thumb, root to tip, as a
/// take held it: a rotation in the rest pose's space, relative to the bone
/// before it.
pub type Captured = [[Rigid; 4]; 5];

/// For each digit's chain, the MediaPipe joint at the same place, or `NONE`
/// (the fingers' metacarpal roots, which MediaPipe does not see).
const NONE: usize = usize::MAX;
const TRACKED: [[usize; 5]; 5] = [[1, 2, 3, 4, NONE], [NONE, 5, 6, 7, 8], [NONE, 9, 10, 11, 12], [NONE, 13, 14, 15, 16], [NONE, 17, 18, 19, 20]];

/// Flesh in front of a fingertip's joint, and in front of the palm's middle,
/// in metres.
pub const PAD: f32 = 0.009;
pub const PALM: f32 = 0.016;

/// The model, read once.
pub struct Model {
    positions: Vec<V3>,
    joints: Vec<[u8; 4]>,
    weights: Vec<[f32; 4]>,
    pub triangles: Vec<[u16; 3]>,
    /// Rest positions of the 25 joints, in the WebXR order.
    rest: Vec<V3>,
    /// Which way the palm faces (-X), from the knuckles.
    palm: V3,
}

/// The WebXR order: wrist, then thumb (4 joints), then four fingers (5 each).
pub const WRIST: usize = 0;
const CHAINS: [&[usize]; 5] = [&[1, 2, 3, 4], &[5, 6, 7, 8, 9], &[10, 11, 12, 13, 14], &[15, 16, 17, 18, 19], &[20, 21, 22, 23, 24]];

impl Model {
    pub fn get() -> &'static Model {
        static MODEL: OnceLock<Model> = OnceLock::new();
        MODEL.get_or_init(|| Model::read(include_bytes!("hand.glb")))
    }

    fn read(glb: &[u8]) -> Model {
        let word = |at: usize| u32::from_le_bytes(glb[at..at + 4].try_into().unwrap()) as usize;
        let json_len = word(12);
        let json: Value = serde_json::from_slice(&glb[20..20 + json_len]).expect("hand.glb: JSON chunk");
        let bin = &glb[20 + json_len + 8..];
        let accessor = |i: u64| -> (&[u8], usize, usize) {
            let a = &json["accessors"][i as usize];
            let view = &json["bufferViews"][a["bufferView"].as_u64().unwrap() as usize];
            let start = view["byteOffset"].as_u64().unwrap_or(0) as usize + a["byteOffset"].as_u64().unwrap_or(0) as usize;
            let width = match a["type"].as_str().unwrap() {
                "SCALAR" => 1,
                "VEC2" => 2,
                "VEC3" => 3,
                "VEC4" => 4,
                "MAT4" => 16,
                t => panic!("hand.glb: accessor type {t}"),
            };
            (&bin[start..], a["count"].as_u64().unwrap() as usize, width)
        };
        let floats = |i: u64| -> Vec<f32> {
            let (b, n, w) = accessor(i);
            (0..n * w).map(|k| f32::from_le_bytes(b[k * 4..k * 4 + 4].try_into().unwrap())).collect()
        };
        let prim = &json["meshes"][0]["primitives"][0];
        let attr = |name: &str| prim["attributes"][name].as_u64().unwrap();

        let positions = floats(attr("POSITION")).chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
        let weights = floats(attr("WEIGHTS_0")).chunks(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
        let (jb, jn, _) = accessor(attr("JOINTS_0"));
        assert_eq!(json["accessors"][attr("JOINTS_0") as usize]["componentType"], 5121, "hand.glb: joints as bytes");
        let joints = (0..jn).map(|k| [jb[k * 4], jb[k * 4 + 1], jb[k * 4 + 2], jb[k * 4 + 3]]).collect();
        let (ib, n, _) = accessor(prim["indices"].as_u64().unwrap());
        let index = |k: usize| u16::from_le_bytes([ib[k * 2], ib[k * 2 + 1]]);
        let triangles = (0..n / 3).map(|t| [index(t * 3), index(t * 3 + 1), index(t * 3 + 2)]).collect();

        // The joints' own nodes sit directly under the root, so their
        // translations are their rest positions.
        let rest: Vec<V3> = json["skins"][0]["joints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| {
                let t = &json["nodes"][n.as_u64().unwrap() as usize]["translation"];
                [0, 1, 2].map(|k| t[k].as_f64().unwrap_or(0.0) as f32)
            })
            .collect();
        assert_eq!(rest.len(), 25, "hand.glb: the 25 WebXR joints");
        let palm = unit(cross(sub(rest[6], rest[WRIST]), sub(rest[21], rest[WRIST])));
        Model { positions, joints, weights, triangles, rest, palm }
    }

    /// Each joint's motion away from the rest pose, for a pose.
    pub fn bones(&self, pose: &Pose) -> [Rigid; 25] {
        let mut out = [Rigid::ID; 25];
        for (digit, chain) in CHAINS.iter().enumerate() {
            let curl = pose.curl[digit];
            // How the curl shares out down the chain, knuckle to tip.
            // The thumb's knuckles face the other way about this axis, so its share
            // is negative: curling still folds it in across the palm.
            let share: &[f32] = if digit == 0 { &[-0.45, -0.65, -0.8] } else { &[0.08, 1.35, 1.7, 1.05] };
            let mut at = Rigid::ID;
            for (k, &j) in chain.iter().enumerate() {
                if k + 1 < chain.len() {
                    let p = self.rest[j];
                    let along = unit(sub(self.rest[chain[k + 1]], p));
                    // Bending turns the bone towards the palm.
                    let axis = unit(cross(along, self.palm));
                    let mut here = Rigid::about(p, axis, curl * share[k]);
                    if let Some(c) = &pose.captured {
                        here = c[digit][k].pivoted(p).then_after(&here);
                    }
                    if k == 1 && digit > 0 {
                        // Spread fans the fingers about the middle one.
                        let fan = (digit as f32 - 2.0) * pose.spread;
                        here = here.then_after(&Rigid::about(p, self.palm, fan));
                    }
                    at = at.then_after(&here);
                }
                out[j] = at;
            }
        }
        out
    }

    /// How each knuckle is bent in one frame of a take (MediaPipe's 21
    /// joints): the take's palm is turned onto the model's, so only how the
    /// fingers move against the palm comes across, and each bone is swung
    /// from where its parent left it to the take's direction. The model keeps
    /// its own bone lengths.
    pub fn captured(&self, take: &[V3; 21]) -> Captured {
        let onto = Rigid::between(
            sub(take[9], take[0]),
            sub(take[5], take[17]),
            sub(self.rest[11], self.rest[WRIST]),
            sub(self.rest[6], self.rest[21]),
        );
        let mut out = [[Rigid::ID; 4]; 5];
        for (digit, chain) in CHAINS.iter().enumerate() {
            let tracked = TRACKED[digit];
            // The bone before's turn, from the rest pose.
            let mut parent = Rigid::ID;
            for k in 0..chain.len() - 1 {
                if tracked[k] == NONE || tracked[k + 1] == NONE {
                    continue;
                }
                let rest = unit(sub(self.rest[chain[k + 1]], self.rest[chain[k]]));
                let want = unit(onto.turn(sub(take[tracked[k + 1]], take[tracked[k]])));
                let swing = Rigid::swing(parent.turn(rest), want);
                out[digit][k] = parent.inverse_turn().then_after(&swing).then_after(&parent);
                parent = swing.then_after(&parent);
            }
        }
        out
    }

    /// The skinned vertices, through `place`.
    pub fn skin(&self, bones: &[Rigid; 25], place: &Rigid, out: &mut Vec<V3>) {
        out.clear();
        for ((v, j), w) in self.positions.iter().zip(&self.joints).zip(&self.weights) {
            let mut p = [0.0; 3];
            for k in 0..4 {
                if w[k] > 0.0 {
                    p = add(p, scale(bones[j[k] as usize].apply(*v), w[k]));
                }
            }
            out.push(place.apply(p));
        }
    }

    /// A joint's posed position, before placing.
    pub fn joint(&self, bones: &[Rigid; 25], j: usize) -> V3 {
        bones[j].apply(self.rest[j])
    }

    /// The rest-pose direction the fingers point (wrist to middle knuckle)
    /// and the way the palm faces.
    pub fn axes(&self) -> (V3, V3) {
        (unit(sub(self.rest[11], self.rest[WRIST])), self.palm)
    }

    /// `pose`, with each digit bent towards `limit` only as far as it must be
    /// so none of its joints (plus `pad` of flesh) comes nearer us than
    /// `wall`, once placed by `place`. Bending towards `limit` must move the
    /// digit back from us: flattening an arched finger, or curling one that
    /// points at us.
    pub fn against(&self, pose: Pose, place: &Rigid, wall: f32, pad: f32, limit: f32) -> Pose {
        let reach = |p: &Pose, digit: usize| {
            let bones = self.bones(p);
            CHAINS[digit][1..].iter().map(|&j| place.apply(self.joint(&bones, j))[2]).fold(f32::MIN, f32::max) + pad
        };
        let mut out = pose;
        for digit in 0..5 {
            if reach(&out, digit) <= wall {
                continue;
            }
            // Bisect between the pose's own bend (too near) and the limit.
            let (mut near, mut far) = (pose.curl[digit], limit);
            out.curl[digit] = far;
            if reach(&out, digit) > wall {
                continue;
            }
            for _ in 0..14 {
                out.curl[digit] = 0.5 * (near + far);
                if reach(&out, digit) > wall {
                    near = out.curl[digit];
                } else {
                    far = out.curl[digit];
                }
            }
            out.curl[digit] = far;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Posed from a take, every tracked bone points where the take's did,
    /// once the take's palm is turned onto the model's.
    #[test]
    fn a_captured_pose_points_its_bones_as_the_take_did() {
        let model = Model::get();
        for clip in crate::patches::veil::clips::all() {
            for i in 0..20 {
                let take = clip.at(i as f32 * 0.37);
                let pose = Pose { curl: [0.0; 5], spread: 0.0, captured: Some(model.captured(&take)) };
                let bones = model.bones(&pose);
                let onto = Rigid::between(
                    sub(take[9], take[0]),
                    sub(take[5], take[17]),
                    sub(model.rest[11], model.rest[WRIST]),
                    sub(model.rest[6], model.rest[21]),
                );
                for (digit, chain) in CHAINS.iter().enumerate() {
                    for k in 0..chain.len() - 1 {
                        let (a, b) = (TRACKED[digit][k], TRACKED[digit][k + 1]);
                        if a == NONE || b == NONE {
                            continue;
                        }
                        let posed = unit(sub(model.joint(&bones, chain[k + 1]), model.joint(&bones, chain[k])));
                        let want = unit(onto.turn(sub(take[b], take[a])));
                        assert!(dot(posed, want) > 0.9998, "digit {digit} bone {k}: off by {:.2} degrees", dot(posed, want).clamp(-1.0, 1.0).acos().to_degrees());
                    }
                }
            }
        }
    }
}
