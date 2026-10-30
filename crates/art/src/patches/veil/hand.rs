//! The hand behind the veil: a real skinned model (`hand.glb`, the WebXR
//! generic right hand, MIT - see `hand.LICENSE`), posed by bending its
//! fingers, then laid against the sheet.
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
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub curl: [f32; 5],
    pub spread: f32,
}

impl Pose {
    /// The index finger out, the rest folded away.
    pub const POINT: Pose = Pose { curl: [0.85, 0.0, 0.95, 1.0, 1.0], spread: 0.02 };
    /// Index and middle out together.
    pub const TWO: Pose = Pose { curl: [0.85, 0.0, 0.05, 0.95, 1.0], spread: 0.03 };
    /// The index crooked, beckoning.
    pub const CROOK: Pose = Pose { curl: [0.85, 0.38, 0.95, 1.0, 1.0], spread: 0.02 };
    /// Flat and spread, as if laid against glass: the fingers bent back a
    /// little, as they are when a hand presses.
    pub const FLAT: Pose = Pose { curl: [0.0, -0.14, -0.14, -0.12, -0.1], spread: 0.22 };
    /// Fingers hooked, grasping at the cloth.
    pub const CLAW: Pose = Pose { curl: [0.3, 0.42, 0.45, 0.45, 0.5], spread: 0.12 };

    pub fn mix(a: Pose, b: Pose, k: f32) -> Pose {
        Pose { curl: std::array::from_fn(|i| a.curl[i] + (b.curl[i] - a.curl[i]) * k), spread: a.spread + (b.spread - a.spread) * k }
    }
}

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
pub const INDEX_TIP: usize = 9;
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

    /// The posed index finger's direction, knuckle to tip.
    pub fn index_dir(&self, bones: &[Rigid; 25]) -> V3 {
        unit(sub(self.joint(bones, INDEX_TIP), self.joint(bones, 6)))
    }
}
