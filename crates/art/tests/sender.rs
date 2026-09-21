//! The art system on the wire, end to end, through `screeny-sim`.
//!
//! This is card 101's acceptance. The claim being checked is the one the whole
//! generative art system is built on (`docs/design/generative-art-brief.md`
//! section 5): a patch that renders a palette and an index plane gets *those
//! pixels* on the panel, and a patch that renders continuous colour gets
//! exactly what the studio's preview drew - not an approximation of it, and
//! not something the preview lied about.
//!
//! The receiver is `screeny-sim` (card 006), the second, independent
//! implementation of the protocol. It owes `screeny-encode` nothing, which is
//! the only reason an exactness claim checked here is worth anything.
//!
//! **No test in this file may touch the bench device.** Loopback, ephemeral
//! ports, mDNS off, and every wait has a deadline.

#![cfg(feature = "sender")]

use std::sync::mpsc::{channel, Receiver, Sender as Tx};
use std::time::{Duration, Instant};

use screeny::{Cadence, Device, LinkConfig, Sent};
use screeny_art::frame::WireFrame;
use screeny_art::output::{Output, SenderOutput};
use screeny_art::panel::Panel;
use screeny_art::patch::{self, local_now, Ctx, Params};
// `pipeline::Output` is spelled out: the sink trait above is also `Output`.
use screeny_art::{pipeline, Measured, Pipeline};
use screeny_sim::{Config, SimDevice};

/// Nothing here waits longer than this.
const PATIENCE: Duration = Duration::from_secs(5);
/// Frames per run. A second's worth: long enough for the codec chooser's
/// hysteresis to matter, short enough that the suite stays fast.
const FRAMES: usize = 30;
/// Gap between frames. The device shows newest-wins with about five frames
/// queued below its socket, so a blast would supersede some of them and there
/// would be nothing to compare.
const GAP: Duration = Duration::from_millis(25);

/// One frame as the *device* saw it.
struct Shown {
    seq: u16,
    codec: u8,
    bytes: usize,
    decoded: Vec<u8>,
}

/// What the device *displays* for the frame it received.
///
/// The datagram carries sRGB8 codes; the panel shows them at 64 duty levels
/// spread over sixteen dither phases, which is the identity above code 38 and
/// a collapse onto a shared level below it (card 102,
/// `screeny_art::panel::Panel`). The studio's preview is drawn through the
/// same model, so this is the transform that makes "the preview is what the
/// panel shows" a statement about the same two pictures.
fn as_shown(decoded: &[u8]) -> Vec<u8> {
    let mut px = decoded.to_vec();
    Panel::DEVICE.show(&mut px);
    px
}

/// Start a simulator on loopback with **ephemeral** ports, mDNS off, and
/// return a `Device` pointing at both of them.
///
/// No port is chosen, guessed or incremented here. `Config::for_test` binds
/// port 0 for each socket and the kernel answers, so nothing in this file can
/// collide with another test process or reach the bench device's 49374/49375
/// even in principle. Card 111 is what makes that possible: `SenderOutput::
/// attach` hands the link a device it has already resolved, control port and
/// all, instead of `Device::from_addr`'s frame + 1 guess.
fn start_sim() -> (SimDevice, Device, Receiver<Shown>) {
    let (tx, rx): (Tx<Shown>, Receiver<Shown>) = channel();
    let sink = Box::new(move |f: &screeny_proto::Rgb888Frame, m: &screeny_sim::FrameMeta| {
        let _ = tx.send(Shown { seq: m.seq, codec: m.codec, bytes: m.bytes, decoded: f.to_vec() });
    });
    let dev = SimDevice::start_with(Config::for_test(), Some(sink)).expect("the simulator starts");
    let device = Device {
        instance: "screeny-sim".into(),
        host: None,
        frame: dev.frame_addr(),
        control: dev.control_addr(),
        addresses: vec![dev.frame_addr().ip()],
        info: None,
    };
    (dev, device, rx)
}

/// What one [`run`] recorded: five parallel lists, one entry per frame -
/// except `shown`, which holds whatever the device managed before the
/// deadline, and is lined up with the rest by sequence number.
struct Run {
    wires: Vec<WireFrame>,
    previews: Vec<Vec<u8>>,
    measured: Vec<Measured>,
    sents: Vec<Sent>,
    shown: Vec<Shown>,
}

/// Render `frames` frames of `id` through the pipeline and send every one of
/// them to a simulator, then collect what the device displayed.
///
/// `Cadence::Free` on purpose. This test feeds frames as fast as it can render
/// them, and the default ceiling would fold that down to the panel's rate -
/// right in a real run and wrong here: the meter and the sender each keep their
/// own encoder, and the
/// chooser's hysteresis means two encoders only agree if they are shown the
/// same frames. One in, one out is what makes "the preview is what the panel
/// shows" a checkable statement rather than a usually-true one.
fn run(id: &str) -> Run {
    let (_dev, device, rx) = start_sim();
    let def = patch::find(id).unwrap_or_else(|| panic!("no patch called `{id}`"));
    let params = Params::defaults(def.params);
    let mut patch = (def.make)(7);

    let mut output = pipeline::Output::default();
    // The limiter is a time-varying gain; leaving it on would be fine but it
    // makes a failure harder to read.
    output.limiter.enabled = false;
    let mut pipeline = Pipeline::new(output);

    let cfg = LinkConfig { cadence: Cadence::Free, ..LinkConfig::default() };
    let mut out =
        SenderOutput::attach(device, cfg).expect("the simulator is on loopback and answers");

    // Measure against the device that is actually connected, exactly as
    // `screeny-art play` does.
    let lim = out.limits();
    assert!(lim.connected, "the link did not come up");
    pipeline.meter().set_limits(lim.budget, lim.codecs.clone());

    let (mut wires, mut previews, mut measured, mut sents) = (vec![], vec![], vec![], vec![]);
    let dt = 1.0 / 30.0;
    let began = local_now();
    for i in 0..FRAMES {
        let t = i as f64 * dt;
        let frame = patch.render(&Ctx { t, dt, now: began + t, params: &params });
        let result = pipeline.process(frame, dt);
        out.send(&result.wire).expect("the network cannot fail a send");
        let sent = out.last_sent().expect("every frame reached the wire under Cadence::Free");
        assert!(sent.is_sent());
        wires.push(result.wire);
        previews.push(result.preview);
        measured.push(result.measured);
        sents.push(sent);
        std::thread::sleep(GAP);
    }

    // Everything the device displayed, up to the deadline.
    let mut shown = Vec::new();
    let deadline = Instant::now() + PATIENCE;
    while shown.len() < FRAMES {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else { break };
        match rx.recv_timeout(left) {
            Ok(s) => shown.push(s),
            Err(_) => break,
        }
    }
    out.close();
    Run { wires, previews, measured, sents, shown }
}

/// Line the device's frames up with ours by sequence number, so a frame lost
/// on loopback shifts nothing.
fn aligned(sents: &[Sent], shown: &[Shown]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    for (j, s) in shown.iter().enumerate() {
        if let Some(i) = sents.iter().position(|t| matches!(t, Sent::Frame { seq, .. } if *seq == s.seq)) {
            pairs.push((i, j));
        }
    }
    pairs
}

/// Expand a palette and an index plane the way the panel must: `palette[i]`,
/// and nothing else. Computed here, independently of the pipeline.
fn expand(palette: &[[u8; 3]], indices: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(indices.len() * 3);
    for &i in indices {
        out.extend_from_slice(&palette[i as usize]);
    }
    out
}

fn report(label: &str, sents: &[Sent], shown: &[Shown]) {
    let bytes: usize = shown.iter().map(|s| s.bytes).sum();
    let exact = sents.iter().filter(|s| s.exact()).count();
    let mut codecs: Vec<&str> = shown.iter().map(|s| screeny::codec_name(s.codec)).collect();
    codecs.sort_unstable();
    codecs.dedup();
    println!(
        "{label}: {} sent, {} displayed, {} exact / {} fallback, {} B/frame mean, codecs {:?}",
        sents.len(),
        shown.len(),
        exact,
        sents.len() - exact,
        bytes / shown.len().max(1),
        codecs,
    );
}

/// An indexed patch arrives **pixel-exact**: every pixel the panel lights is
/// `palette[index]`, as the patch drew it.
#[test]
fn an_indexed_patch_arrives_pixel_exact() {
    // `plasma` was the second of these until card 178. `flock` is the
    // replacement and not a lesser one: it draws a two-dimensional palette -
    // a sky gradient crossed with the birds - and says in its own header that
    // the frame is indexed and exact, which is exactly the claim under test.
    for id in ["clocks-numerals", "flock"] {
        let Run { wires, previews, measured, sents, shown } = run(id);
        report(id, &sents, &shown);
        let pairs = aligned(&sents, &shown);
        assert!(pairs.len() >= FRAMES - 2, "{id}: only {} of {FRAMES} frames arrived", pairs.len());

        for (i, j) in pairs {
            let (palette, indices) = wires[i].indexed.as_ref().unwrap_or_else(|| panic!("{id}: frame {i} is not indexed"));
            assert!(measured[i].exact, "{id}: frame {i} was not sent exactly ({})", measured[i].codec_name());
            assert!(sents[i].exact(), "{id}: the link disagrees with the meter about frame {i}");
            assert_eq!(
                shown[j].decoded,
                expand(palette, indices),
                "{id}: frame {i} would show something other than palette[index]"
            );
            // And therefore also: what the studio drew is what the panel shows.
            assert_eq!(as_shown(&shown[j].decoded), previews[i], "{id}: frame {i}'s preview is not what the panel shows");
        }
    }
}

/// A continuous patch cannot be sent exactly - and the preview shows the
/// damage the codec really does, pixel for pixel, rather than a model of it.
#[test]
fn a_continuous_patch_matches_the_preview() {
    let id = "metaballs";
    let Run { wires, previews, measured, sents, shown } = run(id);
    report(id, &sents, &shown);
    let pairs = aligned(&sents, &shown);
    assert!(pairs.len() >= FRAMES - 2, "{id}: only {} of {FRAMES} frames arrived", pairs.len());

    let mut lossy = 0;
    for (i, j) in pairs {
        assert!(wires[i].indexed.is_none(), "{id} is supposed to be a continuous patch");
        assert_eq!(
            as_shown(&shown[j].decoded), previews[i],
            "{id}: frame {i} reached the panel as something other than the preview"
        );
        if !measured[i].exact {
            lossy += 1;
            assert_ne!(previews[i], wires[i].rgb, "{id}: frame {i} claims to be lossy but nothing changed");
        }
        assert_eq!(shown[j].codec, measured[i].codec, "{id}: frame {i}'s codec is not the one the meter chose");
        assert_eq!(shown[j].bytes as u32, measured[i].bytes, "{id}: frame {i}'s size is not the one the meter measured");
    }
    assert!(lossy > 0, "{id} should have more colours than the exact path can carry");
}

/// The meter and the link are the same encoder asked the same question, so
/// they must never disagree about a frame. If they do, the studio's numbers
/// are decoration.
#[test]
fn the_meter_agrees_with_the_link() {
    for id in ["clocks-numerals", "metaballs"] {
        let Run { measured, sents, .. } = run(id);
        for (i, (m, s)) in measured.iter().zip(&sents).enumerate() {
            let Sent::Frame { codec, bytes, exact, .. } = *s else { panic!("{id}: frame {i} did not reach the wire") };
            assert_eq!(m.codec, codec, "{id}: frame {i} codec");
            assert_eq!(m.bytes, bytes as u32, "{id}: frame {i} bytes");
            assert_eq!(m.exact, exact, "{id}: frame {i} exactness");
        }
    }
}
