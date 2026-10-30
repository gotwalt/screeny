// A gossamer sheet, seen face on, with something pressing through it from the
// far side. The sheet is a heightfield (z towards us) tented over fingertips.
// P: 0 hue, 1 tint (chroma), 2 pallor at the fingertip, 3 finger size, 4 folds
// X: 0 kind (0 none, 1 finger, 2 two, 3 hand), 1-2 tip, 3 push, 4 toward,
//    5 lean, 6 presence, 7 ripple, 8 ripple age, 9-10 ripple centre,
//    11-12 second finger offset, 13 second finger push, 14 hand span over finger size

// Where the cloth is pinned, as a multiple of the finger's radius: the tent
// falls off as a stretched membrane does, logarithmically, out to here.
const PINNED: f32 = 3.0;
// The key light grazes the sheet from above left, so the flat cloth catches
// nothing and only what is pushed out of it is lit.
const KEY: vec3<f32> = vec3<f32>(-0.55, 0.78, 0.3);

fn smax(a: f32, b: f32, k: f32) -> f32 {
  let h = max(k - abs(a - b), 0.0) / k;
  return max(a, b) + h * h * k * 0.25;
}

// The sheet over one digit: a capsule from `tip` back along `back` (unit,
// into the far side) for `len`, radius `r`. Returns height and how much of the
// cloth there is pressed against it.
fn digit(xy: vec2<f32>, tip: vec3<f32>, back: vec3<f32>, len: f32, r: f32) -> vec2<f32> {
  let ba = back.xy * len;
  let bb = dot(ba, ba);
  let s = select(0.0, clamp(dot(xy - tip.xy, ba) / bb, 0.0, 1.0), bb > 1e-6);
  let c = tip + back * len * s;
  let rho = length(xy - c.xy);
  let q = rho / r;
  let cap = select(-9.0, c.z + r * sqrt(max(1.0 - q * q, 0.0)), q < 1.0);
  // Cloth only ever tents towards us; a digit still behind it moves nothing.
  let lift = max(c.z + 0.4 * r, 0.0);
  var tent = lift * clamp(1.0 - log(max(q, 1.0)) / log(PINNED), 0.0, 1.0);

  // Folds radiate from the tip where the cloth is drawn tight over it.
  let d = xy - tip.xy;
  let rt = length(d);
  let ang = atan2(d.y, d.x);
  let band = smoothstep(r * 0.9, r * 1.4, rt) * (1.0 - smoothstep(r * 1.6, r * PINNED, rt));
  let wave = sin(ang * 5.0 + 1.4 * sin(ang * 2.0 + u.seed * 40.0) + rt * 3.0);
  tent += max(tip.z, 0.0) * 0.1 * P(4) * band * wave;

  let h = smax(cap, tent, r * 0.25);
  return vec2<f32>(h, select(0.0, (1.0 - q * q) * smoothstep(0.0, 0.1, c.z), q < 1.0));
}

fn heading(toward: f32, lean: f32) -> vec3<f32> {
  return normalize(vec3<f32>(cos(toward) * sin(lean), sin(toward) * sin(lean), -cos(lean)));
}

// The sheet over one part of a hand: `d` is the distance to the part's
// outline (negative inside), `z` how far it presses, `r` how round its edge is.
fn part(d: f32, z: f32, r: f32) -> vec2<f32> {
  let inside = clamp(-d / r, 0.0, 1.0);
  let cap = select(-9.0, z + r * sqrt(1.0 - (1.0 - inside) * (1.0 - inside)), d < 0.0);
  let tent = max(z + 0.4 * r, 0.0) * clamp(1.0 - log(1.0 + max(d, 0.0) / r) / log(PINNED), 0.0, 1.0);
  return vec2<f32>(smax(cap, tent, r * 0.25), inside * smoothstep(0.0, 0.05, z));
}

fn segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
  let pa = p - a;
  let ba = b - a;
  return length(pa - ba * clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0));
}

// A hand pressed flat against the cloth from behind, palm first, `s` across
// the palm. Its fingers point along X(4); `tip` is the middle of the palm.
fn hand(xy: vec2<f32>, tip: vec3<f32>, s: f32) -> vec2<f32> {
  let up = vec2<f32>(cos(X(4)), sin(X(4)));
  let side = vec2<f32>(up.y, -up.x);
  let q = vec2<f32>(dot(xy - tip.xy, side), dot(xy - tip.xy, up));
  let w = 0.1 * s;

  // The palm: a rounded box, pressing furthest at its heel and middle.
  let pb = abs(q - vec2<f32>(0.0, -0.05 * s)) - vec2<f32>(0.42 * s - w, 0.46 * s - w);
  let dp = length(max(pb, vec2<f32>(0.0))) + min(max(pb.x, pb.y), 0.0) - w;
  var best = part(dp, tip.z, w * 1.8);

  // Four fingers, splayed, each pressing by its own amount, then the thumb.
  for (var i = 0; i < 5; i++) {
    let k = f32(i);
    var base = vec2<f32>((k - 1.5) * 0.22 * s, 0.36 * s);
    var ang = (k - 1.5) * 0.22 + 0.1 * (hash13(vec3<f32>(k, u.seed * 7.0, 1.0)) - 0.5);
    var len = (0.74 - 0.07 * abs(k - 1.3) - select(0.0, 0.14, i == 3)) * s;
    var rad = w * (1.0 - 0.05 * abs(k - 1.5));
    if (i == 4) {
      base = vec2<f32>(0.38 * s, -0.12 * s);
      ang = 1.1;
      len = 0.46 * s;
      rad = w * 1.1;
    }
    let tipq = base + vec2<f32>(sin(ang), cos(ang)) * len;
    let z = tip.z - s * (0.03 + 0.05 * hash13(vec3<f32>(k, u.seed * 9.0, 3.0)));
    let f = part(segment(q, base, tipq) - rad, z, rad);
    best = vec2<f32>(smax(best.x, f.x, w * 0.3), max(best.y, f.y));
  }
  return best;
}

// Height of the sheet and how pressed it is, at xy.
fn sheet(xy: vec2<f32>) -> vec2<f32> {
  let kind = X(0);
  let r = P(3);
  let tip = vec3<f32>(X(1), X(2), X(3));
  var best = vec2<f32>(-9.0, 0.0);

  if (kind > 0.5 && kind < 2.5) {
    best = digit(xy, tip, heading(X(4), X(5)), 1.2, r);
    if (kind > 1.5) {
      let other = vec3<f32>(tip.xy + vec2<f32>(X(11), X(12)), X(13));
      let o = digit(xy, other, heading(X(4), X(5) + 0.1), 1.2, r * 0.92);
      best = vec2<f32>(smax(best.x, o.x, r * 0.5), max(best.y, o.y));
    }
  } else if (kind > 2.5) {
    best = hand(xy, tip, X(14) * r);
  }

  // The sheet itself breathes a little, and shivers once it is let go.
  var flat = 0.012 * sin(xy.x * 1.7 + u.t * 0.31) * sin(xy.y * 2.3 - u.t * 0.23);
  let rr = length(xy - vec2<f32>(X(9), X(10)));
  flat += X(7) * sin(rr * 16.0 - X(8) * 4.5) * exp(-rr * 1.2) * smoothstep(0.0, 0.4, X(8) * 4.5 - rr * 16.0 + 6.0);
  return vec2<f32>(smax(best.x, flat, 0.06), best.y);
}

fn shade(uv: vec2<f32>) -> vec3<f32> {
  // Composed for the Art Frame, which shows x in -1..1.
  if (abs(uv.x) > 1.1) { return vec3<f32>(0.0); }
  let eye = vec3<f32>(0.0, 0.0, 4.0);
  let dir = normalize(vec3<f32>(uv * 0.27, -1.0));
  // March the heightfield from just in front of anything that can stick out.
  // Small steps: the cloth stands nearly vertical where it leaves the finger.
  var d = (eye.z - 1.2) / -dir.z;
  var p = eye + dir * d;
  for (var i = 0; i < 140; i++) {
    p = eye + dir * d;
    let gap = p.z - sheet(p.xy).x;
    if (gap < 0.0005) { break; }
    d += gap * 0.4;
  }

  let e = 0.004;
  let here = sheet(p.xy);
  let gx = (sheet(p.xy + vec2<f32>(e, 0.0)).x - sheet(p.xy - vec2<f32>(e, 0.0)).x) / (2.0 * e);
  let gy = (sheet(p.xy + vec2<f32>(0.0, e)).x - sheet(p.xy - vec2<f32>(0.0, e)).x) / (2.0 * e);
  let n = normalize(vec3<f32>(-gx, -gy, 1.0));
  let l = normalize(KEY);
  let slope = length(vec2<f32>(gx, gy));

  // Only what turns towards the light more than the flat sheet does is lit,
  // so the sheet is black and the form has a hard lit side and a dark one.
  let key = smoothstep(0.0, 0.85, (dot(n, l) - l.z) / (1.0 - l.z));
  // Satin: a soft highlight that slides over the cloth as it moves.
  let gleam = pow(max(dot(n, normalize(l - dir)), 0.0), 18.0) * smoothstep(0.05, 0.3, slope);
  // A faint rim from below right finds the edge of the dark side.
  let rim_l = normalize(vec3<f32>(0.6, -0.75, 0.05));
  let rim = smoothstep(0.15, 0.9, (dot(n, rim_l) - rim_l.z) / (1.0 - rim_l.z));
  // Creases: where the sheet is cupped it sees less of anything.
  let lap = sheet(p.xy + vec2<f32>(0.02, 0.0)).x + sheet(p.xy - vec2<f32>(0.02, 0.0)).x
    + sheet(p.xy + vec2<f32>(0.0, 0.02)).x + sheet(p.xy - vec2<f32>(0.0, 0.02)).x - 4.0 * here.x;
  let open = clamp(1.0 - max(lap, 0.0) * 40.0, 0.35, 1.0);
  // The cloth is thinnest pressed against the fingertip: a little of the
  // finger's own pallor comes through.
  let touch = smoothstep(0.3, 1.0, here.y) * P(2);

  let light = (key * 0.8 + gleam * 0.45 + rim * 0.12) * open + touch * 0.3;
  return oklch(0.97, P(1), P(0)) * light;
}
