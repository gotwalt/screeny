//! What a frame really costs on the wire, and what the panel will really show.
//!
//! This used to be `budget.rs`: a table of estimated payload sizes from the
//! brief, plus a median-cut-and-dither stand-in for "what a lossy codec might
//! do to this". Both are gone. The sender exists now, so we ask it.
//!
//! [`Meter`] runs `screeny-encode`'s chooser - the same code the sender runs,
//! with the same configuration - and then decodes the result through
//! `screeny-proto`, the decoder the *firmware* runs. So `Measured::codec` is
//! the codec that will carry the frame, `Measured::bytes` is the datagram's
//! real payload size, `Measured::exact` says whether the panel gets these
//! pixels or an approximation of them, and [`Meter::decoded`] is the picture
//! the panel will show, not a guess at it.
//!
//! Nothing here opens a socket. `screeny-encode` and `screeny-proto` are pure
//! codec crates, so the studio's meters and preview work with no panel and no
//! network stack; [`crate::output::SenderOutput`] is the part behind a feature.

use crate::frame::WireFrame;
use screeny_encode::{codec_name, EncodeConfig, Encoded, Encoder};
use screeny_proto::{dec, IndexedFrame, Rgb888Frame, NPIX};

/// Pixel payload a frame has to fit into: one datagram, less the 8-byte header.
pub const PAYLOAD_BYTES: u32 = screeny_proto::MAX_PIXEL_PAYLOAD as u32;

/// What the encoder did with one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measured {
    /// Wire codec id (spec 4). [`Measured::codec_name`] puts it in words.
    pub codec: u8,
    /// Pixel payload bytes, against [`PAYLOAD_BYTES`].
    pub bytes: u32,
    /// True when the panel will show these pixels and not a requantised
    /// version of them. An indexed frame that loses this has overflowed the
    /// exact path: too many colours, too little structure.
    pub exact: bool,
    /// Distinct colours in the frame as it was handed over.
    pub colours: u32,
}

impl Measured {
    /// The codec in words: `pal4-lz`, `pal5`, `pal8-lz`, `bc1-dual`, `solid`.
    #[must_use]
    pub fn codec_name(&self) -> &'static str {
        codec_name(self.codec)
    }
}

impl Default for Measured {
    fn default() -> Self {
        Measured { codec: 0, bytes: 0, exact: false, colours: 0 }
    }
}

/// Encodes a frame the way the sender would and decodes it the way the panel
/// would.
///
/// Stateful on purpose: the chooser gives the previous frame's codec a small
/// advantage (spec 4.8's hysteresis), so a meter fed a whole stream answers
/// the same as a sender fed the same stream. Fed a different subset it can
/// differ by a codec on a marginal frame, which is the one thing to know
/// before comparing a meter's answer with a link's: see
/// `crates/art/tests/sender.rs`.
pub struct Meter {
    enc: Encoder,
    budget: usize,
    decoded: Box<Rgb888Frame>,
    idx: Box<[u8; NPIX]>,
    last: Measured,
}

impl Default for Meter {
    fn default() -> Self {
        Meter::new()
    }
}

impl Meter {
    /// A meter at the default budget and the full codec set, which is what an
    /// unconnected studio should assume.
    #[must_use]
    pub fn new() -> Self {
        Meter {
            enc: Encoder::new(EncodeConfig::default()),
            budget: PAYLOAD_BYTES as usize,
            decoded: Box::new([0; NPIX * 3]),
            idx: Box::new([0; NPIX]),
            last: Measured::default(),
        }
    }

    /// Point the meter at the device a link is actually connected to:
    /// `link.limits().budget` and `link.limits().codecs`. Both move under a
    /// running link (spec 6.9), so re-read them rather than caching.
    pub fn set_limits(&mut self, budget: usize, codecs: Vec<u8>) {
        if budget != self.budget {
            self.budget = budget.max(1);
        }
        self.enc.set_codecs(codecs);
    }

    /// The budget in force.
    #[must_use]
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Forget the previous frame's codec, e.g. when the piece changes.
    pub fn reset(&mut self) {
        self.enc.reset();
        self.last = Measured::default();
    }

    /// Encode `wire` as the sender would, and decode the result.
    ///
    /// The indexed arm mirrors `screeny::Sender::send_indexed` and the linear
    /// arm `Sender::send`, because that is the decision being reported.
    pub fn measure(&mut self, wire: &WireFrame) -> Measured {
        let out = match &wire.indexed {
            Some((palette, indices)) if usable_indexed(palette, indices) => {
                self.idx.copy_from_slice(indices);
                let f = IndexedFrame { palette, indices: &self.idx };
                // Infallible: `usable_indexed` has just checked both of the
                // things `encode_indexed` rejects.
                self.enc
                    .encode_indexed(&f, self.budget)
                    .unwrap_or_else(|_| unreachable!("checked by usable_indexed"))
            }
            _ => {
                let f: &Rgb888Frame = wire.rgb.as_slice().try_into().unwrap_or(&[0; NPIX * 3]);
                self.enc.encode(f, self.budget)
            }
        };
        let st = self.enc.last_stats();
        self.last = Measured {
            codec: out.codec,
            bytes: out.payload.len() as u32,
            exact: st.exact,
            colours: distinct_colours(&wire.rgb) as u32,
        };
        self.decode(&out);
        self.last
    }

    /// The last measurement, without re-encoding.
    #[must_use]
    pub fn last(&self) -> Measured {
        self.last
    }

    /// The last measured frame as the panel will decode it: `N * 3` sRGB
    /// bytes. Equal to the handed-over frame when [`Measured::exact`]; the
    /// codec's damage, in full, when it is not.
    #[must_use]
    pub fn decoded(&self) -> &[u8] {
        &self.decoded[..]
    }

    fn decode(&mut self, out: &Encoded) {
        // By construction this cannot fail - the chooser decodes every
        // candidate before picking it - but a black frame is a better answer
        // to a future bug than a panic in a render loop.
        if dec::decode(out.codec, &out.payload, &mut self.decoded).is_err() {
            self.decoded.fill(0);
        }
    }
}

/// True when this palette and index plane can go down the exact path at all.
/// Anything else is the sender's own fallback: expand to RGB and choose.
fn usable_indexed(palette: &[[u8; 3]], indices: &[u8]) -> bool {
    !palette.is_empty()
        && palette.len() <= 256
        && indices.len() == NPIX
        && indices.iter().all(|&i| (i as usize) < palette.len())
}

/// Distinct sRGB colours in an `N * 3` frame.
#[must_use]
pub fn distinct_colours(rgb: &[u8]) -> usize {
    let mut keys: Vec<u32> = rgb
        .chunks_exact(3)
        .map(|c| (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32)
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::N;
    use screeny_proto::dec::codec;

    fn indexed(k: usize, mut f: impl FnMut(usize) -> usize) -> WireFrame {
        let palette: Vec<[u8; 3]> = (0..k).map(|i| [(i * 9) as u8, (i * 5 + 3) as u8, (i * 13) as u8]).collect();
        let indices: Vec<u8> = (0..N).map(|p| (f(p) % k) as u8).collect();
        let mut rgb = Vec::with_capacity(N * 3);
        for &i in &indices {
            rgb.extend_from_slice(&palette[i as usize]);
        }
        WireFrame { rgb, indexed: Some((palette, indices)) }
    }

    /// The claim the whole art system is built on, checked against the real
    /// encoder and the real decoder rather than a table of sizes.
    #[test]
    fn small_palettes_are_exact_and_decode_to_themselves() {
        for k in [2, 16, 17, 32] {
            let mut m = Meter::new();
            let w = indexed(k, |p| p / 7 + p % 5);
            let got = m.measure(&w);
            assert!(got.exact, "{k} colours: not exact");
            assert!(got.bytes <= PAYLOAD_BYTES, "{k} colours: {} bytes", got.bytes);
            assert_eq!(m.decoded(), w.rgb.as_slice(), "{k} colours: the panel would show something else");
        }
    }

    /// Pure index noise is the case the estimates could not model: nothing
    /// compresses, so only the fixed-rate rung can carry it - and it does.
    #[test]
    fn incompressible_indices_still_go_exactly() {
        let mut s = 0x1234_5678_9abc_def0_u64;
        let mut m = Meter::new();
        let w = indexed(32, |_| {
            s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (s >> 33) as usize
        });
        let got = m.measure(&w);
        assert!(got.exact);
        assert_eq!(got.codec, codec::PAL5, "expected the fixed-rate rung, got {}", got.codec_name());
        assert_eq!(got.bytes, 1376);
        assert_eq!(m.decoded(), w.rgb.as_slice());
    }

    /// A continuous frame: not exact, but the meter still knows exactly what
    /// the panel will show, which is what the preview draws.
    #[test]
    fn a_continuous_frame_is_lossy_and_still_fits() {
        let rgb: Vec<u8> = (0..N)
            .flat_map(|i| {
                let (x, y) = (i % 64, i / 64);
                [(x * 4) as u8, (y * 8) as u8, ((x * y) % 256) as u8]
            })
            .collect();
        let mut m = Meter::new();
        let got = m.measure(&WireFrame { rgb: rgb.clone(), indexed: None });
        assert!(got.colours > 32);
        assert!(!got.exact);
        assert!(got.bytes <= PAYLOAD_BYTES);
        assert_ne!(m.decoded(), rgb.as_slice(), "a lossy encode that changed nothing is a bug");
    }

    /// A palette bigger than the guaranteed 32 is exact when it compresses,
    /// which is most flat-shaded work.
    #[test]
    fn a_large_palette_is_exact_when_it_compresses() {
        let mut m = Meter::new();
        let w = indexed(64, |p| p / 32);
        let got = m.measure(&w);
        assert!(got.exact, "64 flat bands should compress");
        assert_eq!(got.codec, codec::PAL8_LZ);
        assert_eq!(m.decoded(), w.rgb.as_slice());
    }

    /// A budget below `PAL5`'s 1376 bytes, with indices that do not compress,
    /// is the one case where an indexed frame cannot go out exactly. It is the
    /// documented fallback: lossy, never silent, never over budget.
    #[test]
    fn a_tiny_budget_falls_back_and_says_so() {
        let mut s = 0xdead_beef_0bad_f00d_u64;
        let mut m = Meter::new();
        m.set_limits(600, dec::SUPPORTED_CODECS.to_vec());
        let w = indexed(32, |_| {
            s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (s >> 33) as usize
        });
        let got = m.measure(&w);
        assert!(!got.exact, "600 bytes cannot hold an incompressible 32-colour frame");
        assert!(got.bytes <= 600, "{} bytes is over the 600-byte budget", got.bytes);
    }
}
