// Physarum: agents on a wrapped trail field, after Jones (2010) and Sage
// Jenson's density-dependent extension. One `step` moves every agent once,
// `diffuse` spreads and fades the trail and banks the deposits, `picture`
// adds what the camera sees into one value per LED.

struct Params {
    w: u32, h: u32, n: u32, seed: u32,
    // Each agent parameter is base + amp * sense^exp, where sense is the
    // trail under the agent relative to the world's mean.
    sd0: f32, sd1: f32, sde: f32,
    sa0: f32, sa1: f32, sae: f32,
    ra0: f32, ra1: f32, rae: f32,
    md0: f32, md1: f32, mde: f32,
    deposit: f32, decay: f32, diffuse: f32, inv_ref: f32,
    // Camera: the world cell at the panel's middle, and cells per LED.
    cx: f32, cy: f32, scale: f32, first: u32,
    wobble: f32,
    // Each substep, this share of agents is scattered afresh.
    respawn: f32,
    // The trail saturates softly toward this, relative to the mean.
    cap: f32,
    // A slow flow over the world, as a pull on every heading (radians per
    // substep at most), and the time that moves it, seconds.
    flow: f32, time: f32, _p0: f32, _p1: f32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> agents: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> trail: array<f32>;
@group(0) @binding(3) var<storage, read_write> dep: array<atomic<u32>>;
@group(0) @binding(4) var<storage, read_write> trail_out: array<f32>;
@group(0) @binding(5) var<storage, read_write> leds: array<f32>;

fn wrap(q: vec2<f32>) -> vec2<f32> {
    let s = vec2(f32(p.w), f32(p.h));
    return q - s * floor(q / s);
}

fn cell(q: vec2<f32>) -> u32 {
    let c = vec2<u32>(wrap(q)) % vec2(p.w, p.h);
    return c.y * p.w + c.x;
}

fn at(q: vec2<f32>) -> f32 {
    return trail[cell(q)];
}

// Bilinear, wrapped.
fn smooth_at(q: vec2<f32>) -> f32 {
    let b = floor(q - 0.5);
    let f = q - 0.5 - b;
    let a = mix(at(b), at(b + vec2(1.0, 0.0)), f.x);
    let c = mix(at(b + vec2(0.0, 1.0)), at(b + vec2(1.0, 1.0)), f.x);
    return mix(a, c, f.y);
}

fn hash(v: u32) -> u32 {
    var x = v * 747796405u + 2891336453u;
    x = ((x >> ((x >> 28u) + 4u)) ^ x) * 277803737u;
    return (x >> 22u) ^ x;
}

fn unit(v: u32) -> f32 {
    return f32(hash(v) & 0xffffu) / 65535.0;
}

fn law(base: f32, amp: f32, e: f32, s: f32) -> f32 {
    return base + amp * pow(max(s, 1e-4), e);
}

fn dir(th: f32) -> vec2<f32> {
    return vec2(cos(th), sin(th));
}

// The flow's direction at `q`: a few waves whose wavelengths divide the
// world, so it is seamless where the world wraps, drifting slowly. It bends
// every straight highway, which on a wrapped world would otherwise close on
// itself along an axis and hold for ever.
fn flow_angle(q: vec2<f32>) -> f32 {
    let u = q / vec2(f32(p.w), f32(p.h)) * 6.2831853;
    let t = p.time;
    let a = sin(2.0 * u.x + 0.031 * t) + sin(3.0 * u.y - 0.023 * t + 1.7)
        + 0.6 * sin(3.0 * u.x + 2.0 * u.y + 0.017 * t + 4.1)
        + 0.6 * sin(5.0 * u.x - 1.0 * u.y - 0.013 * t + 2.3);
    return a * 1.9;
}

@compute @workgroup_size(256)
fn step(@builtin(global_invocation_id) g: vec3<u32>) {
    let i = g.x;
    if (i >= p.n) {
        return;
    }
    let a = agents[i];
    let pos = a.xy;
    var th = a.z;
    let s = at(pos) * p.inv_ref;
    let sd = law(p.sd0, p.sd1, p.sde, s);
    let sa = law(p.sa0, p.sa1, p.sae, s);
    let ra = law(p.ra0, p.ra1, p.rae, s);
    let md = law(p.md0, p.md1, p.mde, s);
    // Each sensor reads one cell, at a random offset within one: a
    // bilinear read on average for a quarter of the cost. A plain
    // nearest-cell read lines agents up on the grid's axes.
    let k = hash(i * 1664525u ^ p.seed);
    let j = vec2(unit(k ^ 0x27d4eb2fu), unit(k ^ 0x165667b1u)) - 0.5;
    let f = at(pos + j + sd * dir(th));
    let l = at(pos + j + sd * dir(th + sa));
    let r = at(pos + j + sd * dir(th - sa));
    let coin = f32(k & 0xffffu) / 65535.0;
    if (f < l || f < r) {
        if (l > r) {
            th += ra;
        } else if (r > l) {
            th -= ra;
        } else {
            th += select(-ra, ra, coin > 0.5);
        }
    }
    th += p.wobble * (f32((k >> 16u) & 0xffffu) / 65535.0 - 0.5);
    th += p.flow * sin(flow_angle(pos) - th);
    var next = wrap(pos + md * dir(th));
    let k2 = hash(k ^ 0x68e31da4u);
    if (f32(k2 & 0xffffu) / 65535.0 < p.respawn) {
        let k3 = hash(k2);
        next = vec2(unit(k3) * f32(p.w), unit(k3 ^ 0x1b873593u) * f32(p.h));
        th = unit(k3 ^ 0x85ebca6bu) * 6.2831853;
    }
    agents[i] = vec4(next, th, a.w);
    atomicAdd(&dep[cell(next)], 1u);
}

@compute @workgroup_size(16, 16)
fn diffuse(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.x >= p.w || g.y >= p.h) {
        return;
    }
    let c = vec2<f32>(g.xy) + 0.5;
    var sum = 0.0;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            sum += at(c + vec2(f32(dx), f32(dy)));
        }
    }
    let k = g.y * p.w + g.x;
    let here = trail[k];
    let banked = f32(atomicExchange(&dep[k], 0u)) * p.deposit;
    let v = mix(here, sum / 9.0, p.diffuse) * (1.0 - p.decay) + banked;
    let cap = p.cap / p.inv_ref;
    trail_out[k] = cap * tanh(v / cap);
}

// One thread per LED: the trail over its footprint, 8 x 8 bilinear samples,
// relative to the world's mean. Added to the frame's exposure; the first
// substep of a frame starts it over.
const TAPS: u32 = 8u;

@compute @workgroup_size(8, 8)
fn picture(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.x >= 64u || g.y >= 32u) {
        return;
    }
    var sum = 0.0;
    let origin = vec2(p.cx, p.cy) + (vec2<f32>(g.xy) - vec2(32.0, 16.0)) * p.scale;
    for (var j = 0u; j < TAPS; j++) {
        for (var i = 0u; i < TAPS; i++) {
            let o = (vec2(f32(i), f32(j)) + 0.5) / f32(TAPS);
            sum += smooth_at(origin + o * p.scale);
        }
    }
    let v = sum / f32(TAPS * TAPS) * p.inv_ref;
    let k = g.y * 64u + g.x;
    leds[k] = select(leds[k], 0.0, p.first == 1u) + v;
}
