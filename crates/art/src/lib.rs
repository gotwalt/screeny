//! Generative art for the screeny panel: 64x32 RGB LEDs, 6-bit linear, one
//! frame per 1464-byte datagram. See `docs/design/generative-art-brief.md`.
//!
//! Patches make frames; the pipeline makes them safe and displayable; outputs
//! take them away.
//!
//! Frame costs and the preview are the real encoder's answers, not estimates
//! ([`meter`]). Talking to a device is [`output::SenderOutput`], behind the
//! `sender` feature - on by default (card 112), because sending is what this
//! crate is for. `--no-default-features` turns it off, and then nothing here
//! opens a socket.

pub mod color;
pub mod dither;
pub mod faces;
pub mod frame;
#[cfg(feature = "gpu")]
pub mod gpu;
pub mod limiter;
pub mod meter;
pub mod output;
pub mod palette;
pub mod panel;
pub mod patch;
pub mod patches;
pub mod pipeline;
pub mod preview;
pub mod rng;
pub mod snapshot;
pub mod variety;

/// **The frame rate. There is one** (card 161).
///
/// The author, 2026-09-20: "i think we can remove 60fps support, as the display
/// really can't do much with it. let's just make everything target 30 with no
/// variability for now." So 30 is what a patch is rendered at, what a player
/// paces itself by, what `screeny-art play|pipe` puts out, what the snapshot
/// tool steps at, and what a browser is shown. Nothing offers a choice.
///
/// It matches the panel's target of 30 fps, which is also the
/// cadence ceiling [`output::SenderOutput`]'s link applies. Rendering above it
/// only fed the link frames it folded away - before this card, half of them.
///
/// Every rate on the art and studio side reads this constant. Three things are
/// deliberately **not** it: `screeny_studio::player::IDLE_FPS`, the 5 fps a
/// player drops to when no panel is connected and nobody is watching; the
/// sender's own loss ladder in `crates/screeny` (spec 6.9), which is protocol
/// behaviour and invisible unless the network is losing frames; and
/// `screeny stream --fps`, the bench tool that measures the device at other
/// rates on purpose.
pub const FPS: f64 = 30.0;

/// Whether this process can render the GPU patches, and why not when it cannot
/// (card 145).
///
/// It exists in both builds on purpose: without the `gpu` feature there is no
/// [`gpu`] module at all, and "this build has no GPU patches" is itself the
/// answer a person deserves instead of a black rectangle.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct GpuStatus {
    /// True when an adapter was opened and the GPU patches will draw.
    pub available: bool,
    /// The adapter's own name. Empty when there is none.
    pub adapter: String,
    /// The backend it came up on: "Metal", "Vulkan", "Gl". Empty when there is
    /// no adapter.
    pub backend: String,
    /// Why there is no adapter, in the words wgpu used. `None` when there is
    /// one.
    pub error: Option<String>,
}

impl GpuStatus {
    /// One line, for a log at startup and for the page. wgpu's own error
    /// already begins "no GPU adapter", so it is passed through rather than
    /// prefixed with a second way of saying the same thing.
    #[must_use]
    pub fn line(&self) -> String {
        match (&self.error, self.available) {
            (_, true) => format!("gpu {} ({})", self.adapter, self.backend),
            (Some(e), _) => e.clone(),
            (None, _) => "no graphics adapter".to_string(),
        }
    }
}

/// The GPU outcome for this process. Decided once; see [`GpuStatus`].
#[must_use]
pub fn gpu_status() -> GpuStatus {
    #[cfg(feature = "gpu")]
    {
        gpu::status()
    }
    #[cfg(not(feature = "gpu"))]
    {
        GpuStatus {
            available: false,
            error: Some("this build has no GPU patches: it was built with --no-default-features".into()),
            ..GpuStatus::default()
        }
    }
}

pub use color::Rgb;
pub use frame::{Frame, WireFrame, H, N, W};
pub use meter::{Measured, Meter};
pub use patch::{Clock, Ctx, Params, Patch, PatchDef};
pub use snapshot::Shot;
pub use pipeline::{Output, Pipeline, Stats};
