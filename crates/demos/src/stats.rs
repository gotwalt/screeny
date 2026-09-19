//! The four numbers the brief says catch most problems before the hardware
//! does: distinct colours, encoded size against the 1464-byte budget, average
//! picture level, and frame-to-frame luminance change.
//!
//! The size is the **real encoder's** (card 016, closing card 071). Card 010
//! shipped an LZSS estimate of its own here and wrote in its log that it
//! disagreed with card 002's lab - it called half the full-colour fractal
//! frames over budget where the lab saw 98-100% of them go out exactly - and
//! that one of the two was wrong. The estimate was: it modelled a generic
//! byte-oriented LZSS, not `PAL4_LZ`/`PAL8_LZ`, and it never tried the block
//! codec or the palette ladder that the sender reaches for when a frame does
//! not fit. Now `frame_stats` runs [`screeny_encode`], which is the code that
//! will actually encode the frame, so "over budget" here means over budget on
//! the wire, and `Wire::Exact` means bit-exact rather than probably-exact.

use screeny_encode::{codec_name, EncodeConfig, Encoder, Profile};
use screeny_panel::Panel;

use crate::color::luma_lin;
use crate::frame::{Frame, NPIX};

/// Bytes of pixel payload one datagram can carry (spec section 1).
pub const BUDGET: usize = screeny_proto::MAX_PIXEL_PAYLOAD;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wire {
    /// Goes on the wire bit-exact, in the named codec.
    Exact(&'static str),
    /// Does not fit losslessly; the sender reduced the palette or block-coded
    /// it, and this is what that costs.
    Lossy,
}

#[derive(Clone, Debug)]
pub struct FrameStats {
    pub colors: usize,
    /// Encoded payload size in bytes. Always `<= BUDGET`: the encoder is not
    /// allowed to overrun, so a frame that cannot fit losslessly comes back
    /// [`Wire::Lossy`] rather than over budget.
    pub est_bytes: usize,
    pub wire: Wire,
    /// Average emitted luminance, 0..1.
    pub apl: f32,
}

impl FrameStats {
    pub fn label(&self) -> String {
        let w = match self.wire {
            Wire::Exact(c) => c,
            Wire::Lossy => "LOSSY",
        };
        format!(
            "{} colors  {} B {}  APL {:.0}%",
            self.colors,
            self.est_bytes,
            w,
            self.apl * 100.0
        )
    }
}

pub fn distinct_colors(f: &Frame) -> usize {
    let mut set = std::collections::HashSet::with_capacity(512);
    for c in f.pixels() {
        set.insert(c);
    }
    set.len()
}

pub fn apl(f: &Frame, p: &Panel) -> f32 {
    let mut acc = 0f32;
    for c in f.pixels() {
        acc += luma_lin(p.emit(c));
    }
    acc / NPIX as f32
}

/// Mean absolute change in emitted luminance between two frames, 0..1. The
/// brief's flash limiter watches this (no full-field flashing above 3 Hz).
pub fn luma_delta(a: &Frame, b: &Frame, p: &Panel) -> f32 {
    let mut acc = 0f32;
    for i in 0..NPIX {
        let ca = [a.px[i * 3], a.px[i * 3 + 1], a.px[i * 3 + 2]];
        let cb = [b.px[i * 3], b.px[i * 3 + 1], b.px[i * 3 + 2]];
        acc += (luma_lin(p.emit(ca)) - luma_lin(p.emit(cb))).abs();
    }
    acc / NPIX as f32
}

/// An encoder configured the way the sender's fast profile is, scoring
/// against `p`.
///
/// The fast profile is card 031's documented shortcut set. It picks the same
/// codec as the full one on everything the art produces, and a preview sheet
/// or a test sweep encodes hundreds of frames.
fn encoder(p: &Panel) -> Encoder {
    Encoder::new(EncodeConfig {
        panel: *p,
        profile: Profile::Fast,
        // One frame at a time here, so the incumbent-codec advantage that
        // smooths a *stream* would just make the answer depend on call order.
        hysteresis: 1.0,
        ..EncodeConfig::default()
    })
}

/// How this frame goes on the wire (protocol-v1 section 4), as encoded by the
/// encoder that will encode it.
pub fn frame_stats(f: &Frame, p: &Panel) -> FrameStats {
    let mut enc = encoder(p);
    let out = enc.encode(f.as_bytes(), BUDGET);
    let st = enc.last_stats();
    FrameStats {
        colors: st.colours,
        est_bytes: out.payload.len(),
        wire: if st.exact {
            Wire::Exact(codec_name(out.codec))
        } else {
            Wire::Lossy
        },
        apl: apl(f, p),
    }
}
