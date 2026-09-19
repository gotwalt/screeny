//! The word clock has to be right at every one of the 288 five-minute slots
//! in a day, fit the panel in all of them, and stay inside its colour budget
//! including mid-transition.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use screeny_demos::clock::{all_slots, layout, phrase, phrase_extent, round5, WordClock};
use screeny_demos::font;
use screeny_demos::frame::{Frame, Indexed, Piece, H, W};
use screeny_demos::panel;
use screeny_demos::stats;
use std::time::Duration;

fn at(h: u32, m: u32, s: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 9, 19)
        .unwrap()
        .and_time(NaiveTime::from_hms_opt(h, m, s).unwrap())
}

/// Independently spelled expectation, written from the rules rather than from
/// the implementation, so the test can disagree with the code.
fn expected(h24: u32, m: u32) -> String {
    const HOURS: [&str; 12] = [
        "TWELVE", "ONE", "TWO", "THREE", "FOUR", "FIVE", "SIX", "SEVEN", "EIGHT", "NINE", "TEN",
        "ELEVEN",
    ];
    const MINUTES: [&str; 7] = [
        "",
        "FIVE",
        "TEN",
        "QUARTER",
        "TWENTY",
        "TWENTY FIVE",
        "HALF",
    ];
    let total = (h24 * 60 + m + 2) / 5 % 288 * 5;
    let (rh, rm) = (total / 60, total % 60);
    if rm == 0 {
        return match rh {
            0 => "MIDNIGHT".into(),
            12 => "NOON".into(),
            _ => format!("{} O'CLOCK", HOURS[(rh % 12) as usize]),
        };
    }
    if rm <= 30 {
        format!(
            "{} PAST {}",
            MINUTES[(rm / 5) as usize],
            HOURS[(rh % 12) as usize]
        )
    } else {
        format!(
            "{} TILL {}",
            MINUTES[((60 - rm) / 5) as usize],
            HOURS[((rh + 1) % 12) as usize]
        )
    }
}

#[test]
fn every_slot_is_spelled_correctly() {
    for (h, m) in all_slots() {
        assert_eq!(phrase(h, m).text(), expected(h, m), "at {h:02}:{m:02}");
    }
}

#[test]
fn every_minute_of_the_day_is_spelled_correctly() {
    // Not just the slots: every minute has to round into the right slot.
    for min in 0..1440u32 {
        let (h, m) = (min / 60, min % 60);
        assert_eq!(phrase(h, m).text(), expected(h, m), "at {h:02}:{m:02}");
    }
}

#[test]
fn the_phrases_the_bench_photo_shows() {
    assert_eq!(phrase(11, 43).text(), "QUARTER TILL TWELVE");
    assert_eq!(phrase(0, 0).text(), "MIDNIGHT");
    assert_eq!(phrase(12, 0).text(), "NOON");
    assert_eq!(phrase(9, 2).text(), "NINE O'CLOCK");
    assert_eq!(phrase(6, 31).text(), "HALF PAST SIX");
    assert_eq!(phrase(13, 35).text(), "TWENTY FIVE TILL TWO");
    assert_eq!(phrase(23, 58).text(), "MIDNIGHT");
    assert_eq!(phrase(12, 58).text(), "ONE O'CLOCK");
    assert_eq!(round5(11, 43), (11, 45));
    assert_eq!(round5(23, 58), (0, 0));
}

#[test]
fn every_phrase_fits_the_panel() {
    let mut widest = (0, String::new());
    for (h, m) in all_slots() {
        let p = phrase(h, m);
        assert!(p.lines.len() <= 3, "{:?} has too many lines", p);
        let w = phrase_extent(&p);
        assert!(
            w <= W as i32,
            "{:?} is {} px wide at {h:02}:{m:02}",
            p.text(),
            w
        );
        for word in layout(&p) {
            assert!(word.x >= 0);
            assert!(
                word.y >= 0 && word.y + font::ROWS as i32 <= H as i32,
                "{:?} line {} runs off the panel",
                p.text(),
                word.line
            );
        }
        if w > widest.0 {
            widest = (w, p.text());
        }
    }
    // The longest case the card calls out must be the binding one, and must
    // still have room to spare.
    assert!(widest.0 <= 60, "widest phrase is {widest:?}");
    println!("widest phrase: {} px, {}", widest.0, widest.1);
}

#[test]
fn stays_within_sixteen_colours_including_mid_transition() {
    let mut worst = 0usize;
    let mut worst_bytes = 0usize;
    for (h, m) in all_slots() {
        // Just after the turn-over, through the roll, and settled.
        for age_ms in [0u64, 120, 260, 330, 420, 560, 700, 900, 4000, 290_000] {
            let mut c = WordClock::at(at(h, m, 0));
            let mut idx = Indexed::default();
            assert!(c.render_indexed(Duration::from_millis(age_ms), &mut idx));
            assert!(
                idx.palette.len() <= 16,
                "palette of {} at {h:02}:{m:02}+{age_ms}ms",
                idx.palette.len()
            );
            let f = idx.to_frame();
            let s = stats::frame_stats(&f, &panel::NOMINAL);
            assert!(
                s.colors <= 16,
                "{} colours at {h:02}:{m:02}+{age_ms}ms",
                s.colors
            );
            assert_eq!(
                s.wire,
                // The name is the sender's own `codec_name` spelling since
                // card 016; the codec it names is the one card 010 asserted.
                stats::Wire::Exact("pal4-lz"),
                "at {h:02}:{m:02}+{age_ms}ms ({} colours, ~{} B)",
                s.colors,
                s.est_bytes
            );
            worst = worst.max(s.colors);
            worst_bytes = worst_bytes.max(s.est_bytes);
        }
    }
    println!("worst case: {worst} colours, ~{worst_bytes} B of 1464");
}

#[test]
fn only_the_words_that_change_move() {
    // 11:40 -> 11:45 replaces the minutes word and nothing else.
    let a = layout(&phrase(11, 40));
    let b = layout(&phrase(11, 45));
    let held: Vec<_> = b.iter().filter(|w| a.contains(w)).collect();
    assert_eq!(held.len(), 2, "TILL and TWELVE should hold still");
    assert!(held.iter().any(|w| w.word == "TILL"));
    assert!(held.iter().any(|w| w.word == "TWELVE"));

    // 11:20 -> 11:25 keeps TWENTY and adds FIVE beside it.
    let a = layout(&phrase(11, 20));
    let b = layout(&phrase(11, 25));
    assert!(b.iter().any(|w| w.word == "FIVE"));
    assert!(
        b.iter().filter(|w| a.contains(w)).count() == 3,
        "TWENTY, PAST and ELEVEN should hold still"
    );
}

#[test]
fn a_moment_renders_the_same_every_time() {
    let mut a = WordClock::at(at(14, 23, 17));
    let mut b = WordClock::at(at(14, 23, 17));
    let mut fa = Frame::black();
    let mut fb = Frame::black();
    for ms in [0u64, 350, 5_000] {
        a.render(Duration::from_millis(ms), &mut fa);
        b.render(Duration::from_millis(ms), &mut fb);
        assert_eq!(fa.px, fb.px, "at +{ms} ms");
    }
}

#[test]
fn the_panel_never_flashes() {
    // Brief section 4: no full-field flashing. The worst frame-to-frame
    // luminance change is during a phrase roll.
    let mut c = WordClock::at(at(11, 42, 59));
    let mut prev = Frame::black();
    let mut worst = 0f32;
    for i in 0..90 {
        let mut f = Frame::black();
        c.render(Duration::from_millis(i * 33), &mut f);
        if i > 0 {
            worst = worst.max(stats::luma_delta(&prev, &f, &panel::NOMINAL));
        }
        prev = f;
    }
    println!("worst frame-to-frame luma delta: {worst:.4}");
    assert!(worst < 0.05, "{worst} is a flash");
}
