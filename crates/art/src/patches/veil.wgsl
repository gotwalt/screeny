// A gossamer sheet, seen face on, with something pressing through it from the
// far side. The sheet is a heightfield (z towards us) tented over fingertips.
// P: 0 hue, 1 tint (chroma), 2 pallor at the fingertip, 3 finger size, 4 folds
// X: 0 kind (0 none, 1 finger, 2 two, 3 hand), 1-2 tip, 3 push, 4 toward,
//    5 lean, 6 presence, 7 ripple, 8 ripple age, 9-10 ripple centre,
//    11-12 second finger offset, 13 second finger push

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
    // A hand laid flat against the cloth: the palm behind, four fingertips and
    // a thumb pressing a little ahead of it, each by its own amount.
    let along = vec2<f32>(cos(X(4)), sin(X(4)));
    let across = vec2<f32>(-along.y, along.x);
    let f = r * 0.62;
    let palm = vec3<f32>(tip.xy + along * 0.36, tip.z - 0.12);
    best = digit(xy, palm, heading(X(4), 0.2), 0.5, 0.3);
    for (var i = 0; i < 5; i++) {
      let k = f32(i);
      var spot = across * (k - 1.5) * f * 2.15 - along * (0.06 - 0.05 * abs(k - 1.6));
      var lean = 1.05;
      if (i == 4) {
        spot = across * 2.8 * f * 1.8 + along * 0.3;
        lean = 1.25;
      }
      let press = tip.z - 0.06 * hash13(vec3<f32>(k, u.seed * 9.0, 3.0)) - select(0.0, 0.07, i == 4);
      let d = digit(xy, vec3<f32>(tip.xy + spot, press), heading(X(4), lean), 0.45, f);
      best = vec2<f32>(smax(best.x, d.x, f * 0.6), max(best.y, d.y));
    }
  }

  // The sheet itself breathes a little, and shivers once it is let go.
  var flat = 0.012 * sin(xy.x * 1.7 + u.t * 0.31) * sin(xy.y * 2.3 - u.t * 0.23);
  let rr = length(xy - vec2<f32>(X(9), X(10)));
  flat += X(7) * sin(rr * 16.0 - X(8) * 4.5) * exp(-rr * 1.2) * smoothstep(0.0, 0.4, X(8) * 4.5 - rr * 16.0 + 6.0);
  return vec2<f32>(smax(best.x, flat, 0.06), best.y);
}

fn shade(uv: vec2<f32>) -> vec3<f32> {
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
