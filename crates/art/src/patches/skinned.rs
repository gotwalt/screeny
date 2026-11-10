//! A skinned, animated glTF model, cut down to its shape by
//! `tools/gltf-shape.py` (the dove's and the goldfish's). It plays the
//! model's first animation as a loop, and can hold it still at any moment of
//! it, blending between the two.

use serde_json::Value;

pub type V3 = [f32; 3];
type M4 = [f32; 16];
type Q = [f32; 4];

/// A node's own transform.
#[derive(Clone, Copy)]
struct Trs {
    t: V3,
    r: Q,
    s: V3,
}

enum Path {
    T,
    R,
    S,
}

struct Channel {
    node: usize,
    path: Path,
    times: Vec<f32>,
    values: Vec<[f32; 4]>,
}

pub struct Model {
    pub positions: Vec<V3>,
    /// Each point's mesh, by its index in `meshes`, and the meshes' names.
    pub part: Vec<u16>,
    pub meshes: Vec<String>,
    joints: Vec<[u16; 4]>,
    weights: Vec<[f32; 4]>,
    pub triangles: Vec<[u32; 3]>,
    parent: Vec<Option<usize>>,
    /// Nodes in an order where every parent comes before its children.
    order: Vec<usize>,
    rest: Vec<Trs>,
    /// Nodes given as a fixed matrix rather than a transform (never animated).
    fixed: Vec<Option<M4>>,
    skin: Vec<usize>,
    inverse_bind: Vec<M4>,
    channels: Vec<Channel>,
    /// The loop: when it starts and how long it lasts (s), back round to
    /// its start.
    start: f32,
    pub length: f32,
}

/// Each joint's transform, ready to skin with.
pub struct Pose(Vec<M4>);

impl Model {
    /// Reads a .glb: every primitive of every mesh, all skinned by its one
    /// skin.
    pub fn read(glb: &[u8]) -> Model {
        let word = |at: usize| u32::from_le_bytes(glb[at..at + 4].try_into().unwrap()) as usize;
        let json_len = word(12);
        let json: Value = serde_json::from_slice(&glb[20..20 + json_len]).expect("glb: JSON chunk");
        let bin = &glb[20 + json_len + 8..];
        let accessor = |i: &Value| -> (&[u8], usize, usize, u64) {
            let a = &json["accessors"][i.as_u64().unwrap() as usize];
            let view = &json["bufferViews"][a["bufferView"].as_u64().unwrap() as usize];
            let start = view["byteOffset"].as_u64().unwrap_or(0) as usize;
            let width = match a["type"].as_str().unwrap() {
                "SCALAR" => 1,
                "VEC3" => 3,
                "VEC4" => 4,
                "MAT4" => 16,
                t => panic!("glb: accessor type {t}"),
            };
            (&bin[start..], a["count"].as_u64().unwrap() as usize, width, a["componentType"].as_u64().unwrap())
        };
        let floats = |i: &Value| -> Vec<f32> {
            let (b, n, w, kind) = accessor(i);
            assert_eq!(kind, 5126, "glb: floats");
            (0..n * w).map(|k| f32::from_le_bytes(b[k * 4..k * 4 + 4].try_into().unwrap())).collect()
        };
        let (mut positions, mut joints, mut weights, mut triangles, mut part, mut meshes) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (m, mesh) in json["meshes"].as_array().unwrap().iter().enumerate() {
            meshes.push(mesh["name"].as_str().unwrap_or("").to_string());
            for prim in mesh["primitives"].as_array().unwrap() {
                let base = positions.len() as u32;
                positions.extend(floats(&prim["attributes"]["POSITION"]).chunks(3).map(|c| [c[0], c[1], c[2]]));
                part.resize(positions.len(), m as u16);
                weights.extend(floats(&prim["attributes"]["WEIGHTS_0"]).chunks(4).map(|c| [c[0], c[1], c[2], c[3]]));
                let (jb, jn, _, kind) = accessor(&prim["attributes"]["JOINTS_0"]);
                joints.extend((0..jn * 4).map(|k| match kind {
                    5121 => jb[k] as u16,
                    5123 => u16::from_le_bytes([jb[k * 2], jb[k * 2 + 1]]),
                    _ => panic!("glb: joints"),
                }).collect::<Vec<_>>().chunks(4).map(|c| [c[0], c[1], c[2], c[3]]));
                let (ib, n, _, kind) = accessor(&prim["indices"]);
                let index = |k: usize| match kind {
                    5121 => ib[k] as u32,
                    5123 => u16::from_le_bytes([ib[k * 2], ib[k * 2 + 1]]) as u32,
                    5125 => u32::from_le_bytes(ib[k * 4..k * 4 + 4].try_into().unwrap()),
                    _ => panic!("glb: indices"),
                };
                triangles.extend((0..n / 3).map(|t| [base + index(t * 3), base + index(t * 3 + 1), base + index(t * 3 + 2)]));
            }
        }

        let nodes = json["nodes"].as_array().unwrap();
        let vec = |v: &Value, d: &[f32]| -> Vec<f32> { v.as_array().map_or(d.to_vec(), |a| a.iter().map(|x| x.as_f64().unwrap() as f32).collect()) };
        let rest = nodes
            .iter()
            .map(|n| {
                let (t, r, s) = (vec(&n["translation"], &[0.0; 3]), vec(&n["rotation"], &[0.0, 0.0, 0.0, 1.0]), vec(&n["scale"], &[1.0; 3]));
                Trs { t: [t[0], t[1], t[2]], r: [r[0], r[1], r[2], r[3]], s: [s[0], s[1], s[2]] }
            })
            .collect();
        let fixed = nodes.iter().map(|n| n.get("matrix").map(|m| vec(m, &[]).try_into().expect("glb: a 4x4 matrix"))).collect();
        let mut parent = vec![None; nodes.len()];
        for (i, n) in nodes.iter().enumerate() {
            for c in n["children"].as_array().into_iter().flatten() {
                parent[c.as_u64().unwrap() as usize] = Some(i);
            }
        }
        let depth = |mut i: usize| {
            let mut d = 0;
            while let Some(p) = parent[i] {
                (i, d) = (p, d + 1);
            }
            d
        };
        let mut order: Vec<usize> = (0..nodes.len()).collect();
        order.sort_by_key(|&i| depth(i));

        let skin_json = &json["skins"][0];
        let skin = skin_json["joints"].as_array().unwrap().iter().map(|j| j.as_u64().unwrap() as usize).collect();
        let inverse_bind = floats(&skin_json["inverseBindMatrices"]).chunks(16).map(|c| c.try_into().unwrap()).collect();

        let anim = &json["animations"][0];
        let mut channels = Vec::new();
        for c in anim["channels"].as_array().unwrap() {
            let sampler = &anim["samplers"][c["sampler"].as_u64().unwrap() as usize];
            assert_eq!(sampler["interpolation"].as_str().unwrap_or("LINEAR"), "LINEAR", "glb: only linear keys");
            let path = match c["target"]["path"].as_str().unwrap() {
                "translation" => Path::T,
                "rotation" => Path::R,
                "scale" => Path::S,
                p => panic!("glb: animates {p}"),
            };
            let width = if matches!(path, Path::R) { 4 } else { 3 };
            let values = floats(&sampler["output"]).chunks(width).map(|v| std::array::from_fn(|k| v.get(k).copied().unwrap_or(0.0))).collect();
            channels.push(Channel { node: c["target"]["node"].as_u64().unwrap() as usize, path, times: floats(&sampler["input"]), values });
        }
        let start = channels.iter().map(|c| c.times[0]).fold(f32::MAX, f32::min);
        let end = channels.iter().map(|c| *c.times.last().unwrap()).fold(f32::MIN, f32::max);
        // The loop's last key runs back into its first a key's time later.
        let key = channels.iter().filter(|c| c.times.len() > 1).map(|c| c.times[1] - c.times[0]).fold(f32::MAX, f32::min);
        Model { positions, part, meshes, joints, weights, triangles, parent, order, rest, fixed, skin, inverse_bind, channels, start, length: end - start + key }
    }

    /// Every node's own transform at `at` (0..1) of the loop.
    fn local(&self, at: f32) -> Vec<Trs> {
        let time = self.start + at.rem_euclid(1.0) * self.length;
        let mut out = self.rest.clone();
        for c in &self.channels {
            let n = c.times.len();
            // After its last key the loop runs back into its first.
            let (a, b, u) = if time >= c.times[n - 1] {
                let u = (time - c.times[n - 1]) / (self.start + self.length - c.times[n - 1]).max(1e-6);
                (c.values[n - 1], c.values[0], u.clamp(0.0, 1.0))
            } else {
                let k = c.times.partition_point(|&x| x <= time).clamp(1, n - 1);
                let (t0, t1) = (c.times[k - 1], c.times[k]);
                (c.values[k - 1], c.values[k], ((time - t0) / (t1 - t0).max(1e-6)).clamp(0.0, 1.0))
            };
            let n = &mut out[c.node];
            match c.path {
                Path::T => n.t = lerp3(a, b, u),
                Path::S => n.s = lerp3(a, b, u),
                Path::R => n.r = nlerp(a, b, u),
            }
        }
        out
    }

    /// The joints at `at` (0..1) of the loop, held `hold` of the way (0..1)
    /// towards the pose at `held`.
    pub fn pose(&self, at: f32, held: f32, hold: f32) -> Pose {
        let mut local = self.local(at);
        if hold > 0.0 {
            let still = self.local(held);
            for (n, s) in local.iter_mut().zip(&still) {
                *n = Trs { t: lerp3v(n.t, s.t, hold), r: nlerp(n.r, s.r, hold), s: lerp3v(n.s, s.s, hold) };
            }
        }
        let mut global = vec![IDENTITY; local.len()];
        for &i in &self.order {
            let own = self.fixed[i].unwrap_or_else(|| trs(&local[i]));
            global[i] = match self.parent[i] {
                Some(p) => mul(&global[p], &own),
                None => own,
            };
        }
        Pose(self.skin.iter().zip(&self.inverse_bind).map(|(&j, ib)| mul(&global[j], ib)).collect())
    }

    /// The mesh's points in a pose.
    pub fn skin(&self, pose: &Pose, out: &mut Vec<V3>) {
        out.clear();
        out.extend(self.positions.iter().zip(&self.joints).zip(&self.weights).map(|((&p, j), w)| {
            let mut q = [0.0; 3];
            for k in 0..4 {
                if w[k] > 0.0 {
                    let r = apply(&pose.0[j[k] as usize], p);
                    for c in 0..3 {
                        q[c] += w[k] * r[c];
                    }
                }
            }
            q
        }));
    }
}

const IDENTITY: M4 = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

fn lerp3(a: [f32; 4], b: [f32; 4], u: f32) -> V3 {
    std::array::from_fn(|k| a[k] + (b[k] - a[k]) * u)
}

fn lerp3v(a: V3, b: V3, u: f32) -> V3 {
    std::array::from_fn(|k| a[k] + (b[k] - a[k]) * u)
}

fn nlerp(a: Q, b: Q, u: f32) -> Q {
    let sign = if a.iter().zip(&b).map(|(x, y)| x * y).sum::<f32>() < 0.0 { -1.0 } else { 1.0 };
    let q: Q = std::array::from_fn(|k| a[k] + (b[k] * sign - a[k]) * u);
    let l = q.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    q.map(|x| x / l)
}

/// Column-major, as glTF has it.
fn trs(n: &Trs) -> M4 {
    let [x, y, z, w] = n.r;
    let r = [
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y + z * w),
        2.0 * (x * z - y * w),
        2.0 * (x * y - z * w),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z + x * w),
        2.0 * (x * z + y * w),
        2.0 * (y * z - x * w),
        1.0 - 2.0 * (x * x + y * y),
    ];
    let s = n.s;
    [
        r[0] * s[0], r[1] * s[0], r[2] * s[0], 0.0,
        r[3] * s[1], r[4] * s[1], r[5] * s[1], 0.0,
        r[6] * s[2], r[7] * s[2], r[8] * s[2], 0.0,
        n.t[0], n.t[1], n.t[2], 1.0,
    ]
}

fn mul(a: &M4, b: &M4) -> M4 {
    std::array::from_fn(|i| {
        let (col, row) = (i / 4, i % 4);
        (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum()
    })
}

fn apply(m: &M4, p: V3) -> V3 {
    std::array::from_fn(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn largest_gap(a: &[V3], b: &[V3]) -> f32 {
        a.iter().zip(b).map(|(p, q)| ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()).fold(0.0, f32::max)
    }

    /// The dove's wingbeat loops: its end runs into its start no more
    /// suddenly than any other step of it.
    #[test]
    fn the_wingbeat_loops_without_a_jump() {
        let m = crate::patches::dove::model();
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let step = 1.0 / 60.0;
        let mut worst_step = 0.0f32;
        for k in 0..60 {
            m.skin(&m.pose(k as f32 * step, 0.0, 0.0), &mut a);
            m.skin(&m.pose((k + 1) as f32 * step, 0.0, 0.0), &mut b);
            worst_step = worst_step.max(largest_gap(&a, &b));
        }
        m.skin(&m.pose(1.0 - 1e-4, 0.0, 0.0), &mut a);
        m.skin(&m.pose(0.0, 0.0, 0.0), &mut b);
        let seam = largest_gap(&a, &b);
        assert!(seam <= worst_step * 0.5 + 1e-4, "the loop jumps {seam} at its seam (a step is at most {worst_step})");
    }
}
