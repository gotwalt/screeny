//! What 64x32 pixels are showing right now: the stream frame, the status
//! screen, the cross-fade between them, and the `IDENTIFY` overlay.
//!
//! Spec section 7.5 for the idle modes, section 6.3 for `IDENTIFY`. This is
//! the one part of the simulator with no wire format in it, so it is also the
//! one part that is allowed to be pretty.

use std::net::Ipv4Addr;

use screeny_proto::control::IdleMode;
use screeny_proto::{Rgb888Frame, H, NBYTES, W};

use crate::config::PanelModel;
use crate::event::State;
use crate::font;
use crate::panel;

/// A 64x32 canvas with the two or three drawing primitives a status screen
/// needs.
pub struct Canvas {
    /// Row-major RGB888, exactly a [`Rgb888Frame`].
    pub px: Box<Rgb888Frame>,
}

impl Default for Canvas {
    fn default() -> Self {
        Canvas {
            px: Box::new([0u8; NBYTES]),
        }
    }
}

impl Canvas {
    /// Fill with one colour.
    pub fn clear(&mut self, c: [u8; 3]) {
        for i in 0..NBYTES {
            self.px[i] = c[i % 3];
        }
    }

    /// Set one pixel, ignoring anything off the panel.
    pub fn set(&mut self, x: i32, y: i32, c: [u8; 3]) {
        if x < 0 || y < 0 || x >= W as i32 || y >= H as i32 {
            return;
        }
        let i = (y as usize * W + x as usize) * 3;
        self.px[i] = c[0];
        self.px[i + 1] = c[1];
        self.px[i + 2] = c[2];
    }

    /// A filled rectangle.
    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: [u8; 3]) {
        for dy in 0..h {
            for dx in 0..w {
                self.set(x + dx, y + dy, c);
            }
        }
    }

    /// Draw a string in the 5x7 font at a 6 px pitch. Returns the width drawn.
    pub fn text_5x7(&mut self, x: i32, y: i32, s: &str, c: [u8; 3]) -> i32 {
        let mut at = x;
        for ch in s.chars() {
            let g = font::glyph_5x7(ch);
            for (col, bits) in g.iter().enumerate() {
                for row in 0..font::H5 {
                    if bits >> row & 1 == 1 {
                        self.set(at + col as i32, y + row as i32, c);
                    }
                }
            }
            at += font::W5 as i32 + 1;
        }
        at - x
    }

    /// Draw a string in the 3x5 font at a 4 px pitch. Characters the tiny
    /// font does not have are skipped.
    pub fn text_3x5(&mut self, x: i32, y: i32, s: &str, c: [u8; 3]) -> i32 {
        let mut at = x;
        for ch in s.chars() {
            if let Some(g) = font::glyph_3x5(ch) {
                for (col, bits) in g.iter().enumerate() {
                    for row in 0..font::H3 {
                        if bits >> row & 1 == 1 {
                            self.set(at + col as i32, y + row as i32, c);
                        }
                    }
                }
            }
            at += font::W3 as i32 + 1;
        }
        at - x
    }
}

/// Everything [`render`] needs to know.
pub struct Scene<'a> {
    /// The last decoded frame, untouched.
    pub decoded: &'a Rgb888Frame,
    /// That frame through the panel model at the current brightness.
    pub panel: &'a Rgb888Frame,
    /// False until a frame has ever been displayed.
    pub have_frame: bool,
    /// The stream state.
    pub state: State,
    /// The idle behaviour in force.
    pub idle_mode: IdleMode,
    /// Now, microseconds since the device started.
    pub now_us: u64,
    /// When the device entered `IDLE`, for the cross-fade.
    pub idle_since_us: u64,
    /// Cross-fade duration.
    pub fade_ms: u32,
    /// Whether the `IDENTIFY` overlay is up.
    pub identify: bool,
    /// Friendly name, for the status screen.
    pub name: &'a str,
    /// Address to print on the status screen.
    pub addr: Ipv4Addr,
    /// Signal strength for the bars.
    pub rssi_dbm: i8,
    /// Brightness currently applied.
    pub brightness: u8,
    /// The panel model, for the `DIM` idle mode.
    pub model: PanelModel,
    /// The provisioning screen, when the device is in `Portal` or `Trial`
    /// (card 224). Drawn by `screeny_provision`, not by this module: the
    /// window and the `--dump-dir` PNGs then show the device's own pixels,
    /// down to the QR's polarity and quiet zone.
    pub provisioning: Option<screeny_provision::Screen<'a>>,
    /// The device is not on a network. The status screen says so instead of
    /// printing an address it does not have (spec section 7.3's "the idle
    /// screen says the network is down").
    pub network_down: bool,
}

/// Compose the frame the panel is scanning out.
pub fn render(s: &Scene<'_>, out: &mut Rgb888Frame) {
    // The portal owns the whole panel while it is up: the device cannot be
    // streaming to somebody who has not told it which network to join, and
    // the QR has to be scannable rather than blended with anything.
    if let Some(screen) = &s.provisioning {
        if screeny_provision::render(screen, out).is_ok() {
            if s.identify {
                let mut c = Canvas::default();
                c.px.copy_from_slice(out);
                identify_overlay(s, &mut c);
                out.copy_from_slice(&c.px[..]);
            }
            return;
        }
        // `Provisioner::screen` never asks for a layout `render` cannot draw,
        // so this is unreachable; falling through to the idle screen is
        // better than a blank panel if it ever becomes reachable.
    }

    // The stream layer: the live frame, or black before the first one.
    let mut live = Canvas::default();
    if s.have_frame {
        live.px.copy_from_slice(s.panel);
    }

    match s.state {
        State::Live | State::Hold => out.copy_from_slice(&live.px[..]),
        State::Idle => {
            let mut target = Canvas::default();
            idle_screen(s, &mut target);
            let t = fade_t(s);
            for (i, o) in out.iter_mut().enumerate() {
                *o = mix(live.px[i], target.px[i], t);
            }
        }
    }

    // Section 6.3: IDENTIFY "overrides the display ... and MUST work in any
    // state, including while another sender holds the lock", so it goes on
    // last and over everything.
    if s.identify {
        let mut c = Canvas::default();
        c.px.copy_from_slice(out);
        identify_overlay(s, &mut c);
        out.copy_from_slice(&c.px[..]);
    }
}

/// 0.0 at the moment `IDLE` was entered, 1.0 once the fade is over.
fn fade_t(s: &Scene<'_>) -> f32 {
    if s.fade_ms == 0 {
        return 1.0;
    }
    let elapsed = s.now_us.saturating_sub(s.idle_since_us) as f32 / 1_000.0;
    (elapsed / s.fade_ms as f32).clamp(0.0, 1.0)
}

fn mix(a: u8, b: u8, t: f32) -> u8 {
    let v = a as f32 + (b as f32 - a as f32) * t;
    v.clamp(0.0, 255.0).round() as u8
}

/// The four idle behaviours of spec section 7.5.
fn idle_screen(s: &Scene<'_>, c: &mut Canvas) {
    match s.idle_mode {
        // HOLD_FOREVER never reaches IDLE, but if a SET_IDLE arrives while
        // the fade is already running, keep showing the frame rather than
        // snapping.
        IdleMode::HoldForever => c.px.copy_from_slice(s.panel),
        IdleMode::Black => c.clear([0, 0, 0]),
        IdleMode::Dim => {
            let dim = (s.brightness as u32 / 10) as u8;
            panel::apply(&s.model, dim, s.decoded, &mut c.px);
        }
        IdleMode::Status => status_screen(s, c),
    }
}

/// Mode 0: "device name, IPv4 address, RSSI bars, a slow ambient animation".
///
/// A black panel is indistinguishable from a broken one, which is why this is
/// the default rather than `BLACK` (spec section 7.5).
fn status_screen(s: &Scene<'_>, c: &mut Canvas) {
    ambient(s, c);

    // Name, 5x7, centred, as many characters as fit.
    let max = W / (font::W5 + 1);
    let name: String = s.name.chars().take(max).collect();
    let w = font::width_5x7(&name) as i32;
    c.text_5x7((W as i32 - w) / 2, 1, &name, [230, 230, 240]);

    // Address, 3x5, centred - or, with no network, what is wrong instead.
    // A panel showing a stale address it can no longer be reached at is
    // worse than one that admits the network is gone.
    let (line, colour) = if s.network_down {
        ("NO NETWORK".to_string(), [255, 150, 40])
    } else {
        (s.addr.to_string(), [90, 180, 255])
    };
    let w = font::width_3x5(&line) as i32;
    c.text_3x5((W as i32 - w) / 2, 11, &line, colour);

    // RSSI bars, five of them, growing to the right. All dark with no link:
    // the last measured RSSI means nothing once the link is gone.
    let bars = if s.network_down { 0 } else { rssi_bars(s.rssi_dbm) };
    let x0 = (W as i32 - (5 * 4 - 1)) / 2;
    for b in 0..5i32 {
        let h = 2 + b * 2;
        let lit = b < bars as i32;
        let col = if lit { [60, 220, 120] } else { [24, 40, 30] };
        c.rect(x0 + b * 4, 28 - h, 3, h, col);
    }
}

/// Five bars from -90 dBm (one) to -50 dBm (five).
#[must_use]
pub fn rssi_bars(rssi_dbm: i8) -> u8 {
    match rssi_dbm {
        r if r >= -55 => 5,
        r if r >= -65 => 4,
        r if r >= -72 => 3,
        r if r >= -80 => 2,
        _ => 1,
    }
}

/// A slow, dim aurora across the background. Deliberately low contrast: the
/// status screen has to be readable, and the panel is in someone's room.
fn ambient(s: &Scene<'_>, c: &mut Canvas) {
    let t = s.now_us as f32 / 1_000_000.0;
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / W as f32;
            let fy = y as f32 / H as f32;
            let v = ((fx * 6.0 + t * 0.35).sin() + (fy * 4.0 - t * 0.21).sin()) * 0.5;
            let v = (v * 0.5 + 0.5).clamp(0.0, 1.0);
            let r = (v * 10.0) as u8;
            let g = (v * 16.0) as u8;
            let b = (16.0 + v * 26.0) as u8;
            c.set(x as i32, y as i32, [r, g, b]);
        }
    }
}

/// Section 6.3's "which one is this?" button: a high-contrast pattern plus
/// the device name and IP, over whatever else is on the panel.
fn identify_overlay(s: &Scene<'_>, c: &mut Canvas) {
    // 4 Hz checkerboard. Nothing else on a shelf does this.
    let phase = (s.now_us / 125_000).is_multiple_of(2);
    for y in 0..H as i32 {
        for x in 0..W as i32 {
            let on = ((x / 2 + y / 2) % 2 == 0) == phase;
            c.set(x, y, if on { [255, 255, 255] } else { [0, 0, 0] });
        }
    }
    // A dark plate so the text is legible over the flashing.
    c.rect(0, 8, W as i32, 16, [0, 0, 0]);
    let max = W / (font::W5 + 1);
    let name: String = s.name.chars().take(max).collect();
    let w = font::width_5x7(&name) as i32;
    c.text_5x7((W as i32 - w) / 2, 9, &name, [255, 200, 40]);
    let addr = s.addr.to_string();
    let w = font::width_3x5(&addr) as i32;
    c.text_3x5((W as i32 - w) / 2, 18, &addr, [255, 255, 255]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn scene<'a>(
        decoded: &'a Rgb888Frame,
        panel: &'a Rgb888Frame,
        state: State,
        idle_mode: IdleMode,
    ) -> Scene<'a> {
        Scene {
            decoded,
            panel,
            have_frame: true,
            state,
            idle_mode,
            now_us: 0,
            idle_since_us: 0,
            fade_ms: 500,
            identify: false,
            name: "sim",
            addr: Ipv4Addr::new(192, 0, 2, 7),
            rssi_dbm: -55,
            brightness: 255,
            model: PanelModel::default(),
            provisioning: None,
            network_down: false,
        }
    }

    #[test]
    fn live_shows_the_frame_untouched() {
        let d = [7u8; NBYTES];
        let p = [9u8; NBYTES];
        let mut out = [0u8; NBYTES];
        render(&scene(&d, &p, State::Live, IdleMode::Status), &mut out);
        assert_eq!(out, p, "LIVE is the panel-model frame and nothing else");
    }

    #[test]
    fn hold_keeps_the_last_frame_lit() {
        let d = [7u8; NBYTES];
        let p = [9u8; NBYTES];
        let mut out = [0u8; NBYTES];
        render(&scene(&d, &p, State::Hold, IdleMode::Status), &mut out);
        assert_eq!(out, p);
    }

    #[test]
    fn the_fade_starts_at_the_frame_and_ends_at_the_idle_screen() {
        let d = [200u8; NBYTES];
        let p = [200u8; NBYTES];
        let mut at_start = [0u8; NBYTES];
        let mut s = scene(&d, &p, State::Idle, IdleMode::Black);
        render(&s, &mut at_start);
        assert_eq!(at_start, p, "t=0 is still the frame");

        s.now_us = 250_000;
        let mut halfway = [0u8; NBYTES];
        render(&s, &mut halfway);
        assert!(halfway[0] > 0 && halfway[0] < 200, "got {}", halfway[0]);

        s.now_us = 500_000;
        let mut done = [0u8; NBYTES];
        render(&s, &mut done);
        assert_eq!(done, [0u8; NBYTES], "BLACK is black once the fade is over");
    }

    #[test]
    fn dim_is_darker_than_the_frame_but_not_black() {
        let d = [255u8; NBYTES];
        let p = [255u8; NBYTES];
        let mut out = [0u8; NBYTES];
        let mut s = scene(&d, &p, State::Idle, IdleMode::Dim);
        s.now_us = 1_000_000;
        render(&s, &mut out);
        assert!(out[0] > 0, "DIM holds the frame, it does not extinguish it");
        assert!(out[0] < 200, "but it is clearly dimmer: {}", out[0]);
    }

    #[test]
    fn the_status_screen_draws_something_readable() {
        let d = [0u8; NBYTES];
        let p = [0u8; NBYTES];
        let mut out = [0u8; NBYTES];
        let mut s = scene(&d, &p, State::Idle, IdleMode::Status);
        s.now_us = 10_000_000;
        s.have_frame = false;
        render(&s, &mut out);
        let bright = out
            .chunks(3)
            .filter(|px| px[0] > 200 && px[1] > 200)
            .count();
        assert!(
            bright > 10,
            "expected the name in near-white, got {bright} px"
        );
    }

    #[test]
    fn identify_overrides_every_state() {
        let d = [0u8; NBYTES];
        let p = [0u8; NBYTES];
        for state in [State::Live, State::Hold, State::Idle] {
            let mut out = [0u8; NBYTES];
            let mut s = scene(&d, &p, state, IdleMode::Status);
            s.identify = true;
            render(&s, &mut out);
            let white = out.chunks(3).filter(|px| px == &[255, 255, 255]).count();
            assert!(white > 100, "{state:?}: expected a bright pattern");
        }
    }

    #[test]
    fn rssi_maps_onto_five_bars() {
        assert_eq!(rssi_bars(-30), 5);
        assert_eq!(rssi_bars(-55), 5);
        assert_eq!(rssi_bars(-60), 4);
        assert_eq!(rssi_bars(-70), 3);
        assert_eq!(rssi_bars(-78), 2);
        assert_eq!(rssi_bars(-95), 1);
    }
}
