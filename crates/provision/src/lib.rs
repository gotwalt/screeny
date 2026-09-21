//! WiFi provisioning, minus the radio.
//!
//! The part of "the device is not on a network yet" that can be right before
//! it ever meets hardware: the join/portal state machine as a pure function
//! of events and time, the `WIFI:` QR payload with its length rule, and the
//! portal screen the panel shows. `no_std`, no allocation, no float, no
//! clock, no I/O. The firmware (card 223) and the simulator (card 224) both
//! drive it, so the behaviour cannot drift between them the way two copies of
//! it would.
//!
//! It is the same split [`screeny_receiver`](https://docs.rs/) made for the
//! receive rules, and it buys the same thing: "three attempts, then the
//! portal, and retry at 3 a.m. if nobody is standing on it" is a unit test
//! that runs in microseconds.
//!
//! # The three pieces
//!
//! | module | what |
//! |---|---|
//! | [`machine`] | [`Provisioner`]: `Boot`/`Joining`/`Online`/`Portal`/`Trial`, events in, [`Action`]s out |
//! | [`button`] | the button's gesture recogniser (card 230): levels and milliseconds in, `ShortPress`/`HoldTick`/`WipeWifi` out |
//! | [`uri`] | the `WIFI:` URI, its ZXing escaping, and the 14-character SSID limit a version 2-L QR imposes |
//! | [`qr`], [`screen`] | the version 2-L encoder and the two portal layouts, drawn into a [`screeny_proto::Rgb888Frame`] |
//!
//! # What this crate will not do
//!
//! * **Hold a password.** [`Event::CredentialsPosted`] carries the SSID and
//!   nothing else; the caller keeps the credential and writes it when the
//!   machine answers [`Action::CommitCredentials`]. Spec section 8.4's "the
//!   PSK never appears in a reply, a log line or on the panel" is therefore
//!   structural here: no type in this crate has a field for it.
//! * **Change the QR.** Version 2, ECC L, byte mode, one LED per module,
//!   standard polarity, a 3-pixel lit quiet zone. Those five numbers were
//!   measured against the owner's phone on the real panel on 2026-09-19 and
//!   are not up for tuning; `tests/render.rs` decodes the rendered frame back
//!   with an independent decoder so they cannot rot quietly.
//! * **Read a clock.** Every method that needs the time is given `now_ms`,
//!   and every comparison is wrapping, because a `u32` of milliseconds wraps
//!   every 49.7 days and this device is meant to run for months.
//!
//! # Example
//!
//! ```
//! use screeny_provision::{Action, Config, Event, JoinTarget, Provisioner, State};
//!
//! let mut p = Provisioner::new(&Config {
//!     ap_ssid: "screeny-4a00a4",
//!     has_stored: false,
//!     ..Config::default()
//! });
//! // Nothing in the store and no compile-time credentials: straight to the
//! // portal, and the caller is told to raise the soft-AP.
//! let actions = p.step(Event::Boot, 0);
//! assert_eq!(actions.as_slice(), &[Action::RaiseAp]);
//! assert_eq!(p.state(), State::Portal);
//!
//! // Somebody posts credentials. Nothing is committed yet.
//! let actions = p.step(Event::CredentialsPosted { ssid: "Example-Wifi1" }, 1_000);
//! assert_eq!(
//!     actions.as_slice(),
//!     &[Action::StartJoin { which: JoinTarget::Trial, attempt: 1 }]
//! );
//! assert_eq!(p.state(), State::Trial);
//!
//! // It works: now, and only now, the credentials are written.
//! let actions = p.step(Event::Joined { ip: [192, 168, 7, 221] }, 4_000);
//! assert_eq!(
//!     actions.as_slice(),
//!     &[
//!         Action::CommitCredentials { which: JoinTarget::Trial },
//!         Action::Announce,
//!     ]
//! );
//! assert!(p.ap_up(), "the AP is held up so the page can report the address");
//! ```

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod button;
pub mod machine;
pub mod qr;
pub mod screen;
pub mod uri;

pub use machine::{
    Action, Actions, Config, Event, FailReason, JoinTarget, Provisioner, State, Timing, Trial,
    TrialOrigin, TrialOutcome, CONNECTED_SCREEN_MS, MAX_ACTIONS, SSID_MAX,
};
pub use qr::{Qr, QrError, QR_MODULES, QR_VERSION};
pub use screen::{
    render, wants_fixed_brightness, Layout, RenderError, Screen, PORTAL_IP, QUIET, TEXT_COLS,
};
pub use uri::{fits, wifi_uri, UriError, UriForm, SSID_MAX_NOPASS, SSID_MAX_SHORT};
