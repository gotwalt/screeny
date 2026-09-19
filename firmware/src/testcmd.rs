//! **Throwaway bench receiver. Card 008 deletes this whole file.**
//!
//! There is no protocol here worth the name. It exists so that a bring-up
//! session can change one thing at a time — brightness, gamma, dither, test
//! pattern — without a two-minute reflash between every camera capture, and
//! so that a full 64x32 frame can be pushed from a five-line Python script
//! before `crates/proto` exists.
//!
//! Everything on UDP 49374. A datagram either starts with the magic `SX` and
//! is a command, or it is raw sRGB888 pixels starting at pixel 0, which is
//! what the card 001 spike accepted and what card 001's notes describe.
//!
//! ```text
//! "SX" 'P' n        show built-in pattern n (0..=7, see `patterns`)
//! "SX" 'C' -        release the pattern, go back to stream/status
//! "SX" 'B' n        brightness 0..=255 (output-enable duty, power-capped)
//! "SX" 'O' n        RAW output-enable slots, ignoring the cap. BENCH ONLY:
//!                   reverts after 10 s. Use only with sparse patterns.
//! "SX" 'G' 0|1      sRGB gamma on/off
//! "SX" 'D' 0|1      temporal dithering on/off
//! "SX" 'F' hi lo …  sRGB888 pixels starting at pixel (hi<<8)|lo
//! ```
//!
//! The magic can collide with pixel data, obviously. That is the sort of
//! thing you accept in code with a deletion date on it.

use core::sync::atomic::Ordering;

use log::info;

use crate::display::NPIX;
use crate::patterns::{self, Pattern};
use crate::{
    now_ms, BRIGHTNESS, BRIGHTNESS_DIRTY, DITHER_ON, FRAME, FRAMES_DROPPED, FRAMES_RECEIVED,
    FRAME_SEQ, GAMMA_ON, LAST_FRAME_MS, OE_OVERRIDE, OE_OVERRIDE_DEADLINE_MS, PATTERN_HOLD,
};

const MAGIC: &[u8; 2] = b"SX";
const OVERRIDE_MS: u32 = 10_000;

pub async fn handle(data: &[u8]) {
    if data.len() >= 4 && &data[0..2] == MAGIC {
        command(data).await;
    } else {
        pixels(data, 0).await;
    }
}

async fn command(data: &[u8]) {
    let op = data[2];
    let arg = data[3];
    match op {
        b'P' => {
            if let Some(p) = Pattern::from_u8(arg) {
                PATTERN_HOLD.store(arg + 1, Ordering::Relaxed);
                let mut frame = FRAME.lock().await;
                patterns::draw(&mut frame, p);
                drop(frame);
                FRAME_SEQ.fetch_add(1, Ordering::Relaxed);
                info!("test: pattern {}", arg);
            }
        }
        b'C' => {
            PATTERN_HOLD.store(0, Ordering::Relaxed);
            info!("test: pattern released");
        }
        b'B' => {
            BRIGHTNESS.store(arg, Ordering::Relaxed);
            OE_OVERRIDE.store(u8::MAX, Ordering::Relaxed);
            BRIGHTNESS_DIRTY.store(2, Ordering::Relaxed);
            info!(
                "test: brightness {} -> {} OE slots",
                arg,
                crate::display::slots_for(arg)
            );
        }
        b'O' => {
            OE_OVERRIDE.store(arg, Ordering::Relaxed);
            OE_OVERRIDE_DEADLINE_MS.store(now_ms().wrapping_add(OVERRIDE_MS), Ordering::Relaxed);
            BRIGHTNESS_DIRTY.store(2, Ordering::Relaxed);
            info!("test: RAW OE slots {} for {} ms", arg, OVERRIDE_MS);
        }
        b'G' => {
            GAMMA_ON.store(arg != 0, Ordering::Relaxed);
            FRAME_SEQ.fetch_add(1, Ordering::Relaxed);
            info!("test: gamma {}", arg != 0);
        }
        b'D' => {
            DITHER_ON.store(arg != 0, Ordering::Relaxed);
            FRAME_SEQ.fetch_add(1, Ordering::Relaxed);
            info!("test: dither {}", arg != 0);
        }
        b'F' => {
            if data.len() >= 5 {
                let offset = ((data[3] as usize) << 8) | data[4] as usize;
                pixels(&data[5..], offset).await;
            }
        }
        _ => {}
    }
}

/// Writes sRGB888 triples into the frame starting at pixel `offset`.
///
/// Newest wins: if the display task is holding the frame lock we drop the
/// datagram rather than queue it, because a late frame is worth less than the
/// next one. A pattern being held wins over the stream, so that a test card
/// stays up while something else is still sending.
async fn pixels(data: &[u8], offset: usize) {
    FRAMES_RECEIVED.fetch_add(1, Ordering::Relaxed);
    LAST_FRAME_MS.store(now_ms(), Ordering::Relaxed);
    if PATTERN_HOLD.load(Ordering::Relaxed) != 0 {
        return;
    }
    match FRAME.try_lock() {
        Ok(mut frame) => {
            let n = (data.len() / 3).min(NPIX.saturating_sub(offset));
            for i in 0..n {
                frame.px[offset + i] = [data[i * 3], data[i * 3 + 1], data[i * 3 + 2]];
            }
            // A partial frame that starts at 0 clears the tail, so the old
            // spike behaviour (490 pixels of a 2048-pixel panel) still looks
            // like it did. A chunk at an offset does not: that is how a whole
            // frame gets assembled out of several datagrams.
            if offset == 0 && n < NPIX {
                for px in frame.px.iter_mut().skip(n) {
                    *px = [0, 0, 0];
                }
            }
            drop(frame);
            FRAME_SEQ.fetch_add(1, Ordering::Relaxed);
        }
        Err(_) => {
            FRAMES_DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}
