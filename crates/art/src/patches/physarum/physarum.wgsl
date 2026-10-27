// Physarum: agents on a wrapped trail field, after Jones (2010) and Sage
// Jenson's density-dependent extension. One `step` moves every agent once,
// `diffuse` spreads and fades the trail, banks the deposits and lets the
// food breathe its scent into it, `picture` adds what the panel sees into
// one value per LED.
//
// The trail is a vec4 per cell: x the trail itself; y and z which way the
// agents that laid it were lying, as (cos 2a, sin 2a) - an orientation, so
// traffic both ways along a vein adds up rather than cancelling; w the
// food's scent, which agents smell with the trail but the panel never
// shows - the colony draws what the food is, not the food itself.

struct Params {
    w: u32, h: u32, n: u32, seed: u32,
    // Each agent parameter is base + amp * sense^exp, where sense is the
    // trail under the agent relative to the world's mean.
    sd0: f32, sd1: f32, sde: f32,
    sa0: f32, sa1: f32, sae: f32,
    ra0: f32, ra1: f32, rae: f32,
    md0: f32, md1: f32, mde: f32,
    deposit: f32, decay: f32, diffuse: f32, inv_ref: f32,
    // The panel's window: the world cell at its top-left, and cells per LED.
    ox: f32, oy: f32, scale: f32, first: u32,
    wobble: f32,
    // Each substep, this share of agents is scattered afresh.
    respawn: f32,
    // The trail saturates softly toward this, relative to the mean.
    cap: f32,
    // How much scent the food breathes into the trail per substep,
    // relative to the mean; the food grid is `food_k` world cells a cell.
    emit: f32,
    food_k: u32, _p0: u32, _p1: u32, _p2: u32,
};

// Fixed point for the deposits.
const ONE: f32 = 256.0;

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> agents: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> trail: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> dep: array<atomic<i32>>;
@group(0) @binding(4) var<storage, read_write> trail_out: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> leds: array<vec4<f32>>;
@group(0) @binding(6) var<storage, read> food: array<f32>;

fn wrap(q: vec2<f32>) -> vec2<f32> {
    let s = vec2(f32(p.w), f32(p.h));
    return q - s * floor(q / s);
}

fn cell(q: vec2<f32>) -> u32 {
    let c = vec2<u32>(wrap(q)) % vec2(p.w, p.h);
    return c.y * p.w + c.x;
}

// What an agent smells: trail and scent.
fn at(q: vec2<f32>) -> f32 {
    let v = trail[cell(q)];
    return v.x + v.w;
}

fn at4(q: vec2<f32>) -> vec4<f32> {
    return trail[cell(q)];
}

// Bilinear, wrapped, all four channels.
fn smooth_at(q: vec2<f32>) -> vec4<f32> {
    let b = floor(q - 0.5);
    let f = q - 0.5 - b;
    let a = mix(at4(b), at4(b + vec2(1.0, 0.0)), f.x);
    let c = mix(at4(b + vec2(0.0, 1.0)), at4(b + vec2(1.0, 1.0)), f.x);
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
    var next = wrap(pos + md * dir(th));
    let k2 = hash(k ^ 0x68e31da4u);
    if (f32(k2 & 0xffffu) / 65535.0 < p.respawn) {
        let k3 = hash(k2);
        next = vec2(unit(k3) * f32(p.w), unit(k3 ^ 0x1b873593u) * f32(p.h));
        th = unit(k3 ^ 0x85ebca6bu) * 6.2831853;
    }
    agents[i] = vec4(next, th, a.w);
    let c = cell(next);
    atomicAdd(&dep[3u * c], i32(ONE));
    atomicAdd(&dep[3u * c + 1u], i32(ONE * cos(2.0 * th)));
    atomicAdd(&dep[3u * c + 2u], i32(ONE * sin(2.0 * th)));
}

@compute @workgroup_size(16, 16)
fn diffuse(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.x >= p.w || g.y >= p.h) {
        return;
    }
    let c = vec2<f32>(g.xy) + 0.5;
    var sum = vec4(0.0);
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            sum += at4(c + vec2(f32(dx), f32(dy)));
        }
    }
    let k = g.y * p.w + g.x;
    let here = trail[k];
    let banked = vec4(
        f32(atomicExchange(&dep[3u * k], 0)),
        f32(atomicExchange(&dep[3u * k + 1u], 0)),
        f32(atomicExchange(&dep[3u * k + 2u], 0)),
        0.0,
    ) * (p.deposit / ONE);
    let fw = p.w / p.food_k;
    let scent = food[(g.y / p.food_k) * fw + g.x / p.food_k] * p.emit / p.inv_ref;
    let v = mix(here, sum / 9.0, p.diffuse) * (1.0 - p.decay) + banked + vec4(0.0, 0.0, 0.0, scent);
    // Saturate the trail softly; the orientation keeps its share of it.
    let cap = p.cap / p.inv_ref;
    let x = cap * tanh(v.x / cap);
    let kept = select(1.0, x / v.x, v.x > 1e-6);
    trail_out[k] = vec4(x, v.y * kept, v.z * kept, v.w);
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
    var sum = vec4(0.0);
    let origin = vec2(p.ox, p.oy) + vec2<f32>(g.xy) * p.scale;
    for (var j = 0u; j < TAPS; j++) {
        for (var i = 0u; i < TAPS; i++) {
            let o = (vec2(f32(i), f32(j)) + 0.5) / f32(TAPS);
            sum += smooth_at(origin + o * p.scale);
        }
    }
    let v = sum / f32(TAPS * TAPS) * p.inv_ref;
    let k = g.y * 64u + g.x;
    leds[k] = select(leds[k], vec4(0.0), p.first == 1u) + v;
}
