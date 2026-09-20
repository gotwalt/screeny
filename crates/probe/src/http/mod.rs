//! The device's **HTTP** conformance suite (card 228).
//!
//! The bench one-liner, and the thing to run after every flash:
//!
//! ```text
//! cargo run --release -p screeny-probe -- --addr 192.168.7.221 http
//! ```
//!
//! One rule per thing an outside observer can check about the HTTP API, each
//! citing the section of `docs/design/device-web.md` or the item of
//! `crates/device-api` it comes from, run against *any* server that claims to
//! serve that API: `screeny-sim` on loopback, or the firmware on the bench.
//! It is the HTTP half of what `suite/` is for the UDP wire, and it keeps the
//! same discipline:
//!
//! * **Rules are data.** A number, a section, the route, the citation, a time
//!   estimate and a function. `--list` prints them, `--only` filters them.
//! * **Replies are parsed with `screeny_device_api`'s own types**, never with
//!   a hand-rolled shape and never with a bare `Value`. The suite defines no
//!   shapes of its own, so "it parses" means the Studio and the firmware
//!   agree about it.
//! * One line per rule, `PASS` / `FAIL` / `SKIP <reason>`, and a non-zero exit
//!   status if anything failed.
//!
//! # Safety: this is pointed at the real panel
//!
//! By default the suite does nothing that takes the device off its network,
//! reboots it, interrupts a stream or writes to flash:
//!
//! * `POST /api/v1/wifi` is only ever sent a body that **cannot** start a join
//!   (no `ssid` field), unless [`Opts::allow_wifi_trial`] is set.
//! * `POST /api/v1/reboot` is only ever sent an unconfirmed body, unless
//!   [`Opts::allow_reboot`] is set.
//! * `POST /api/v1/firmware` is only ever sent a body that cannot pass the
//!   first check of research 006 (empty, or not `0xE9`), so nothing is ever
//!   installed and there is no `--allow-firmware`: a suite has no business
//!   uploading an image.
//! * Brightness is only ever stepped **down** from what the suite found,
//!   unless [`Opts::cap_probe`] is set - the same flag, and the same reason,
//!   as the UDP suite's `--cap-probe`.
//! * Everything it changes (name, brightness, idle mode, the identify overlay)
//!   is put back on **every** exit path: a normal return, a failure, a panic
//!   and ctrl-c. The last line says what it restored to.
//!
//! # The two servers, and the differences that are known
//!
//! The firmware (card 222) and the simulator (224) implement this API twice.
//! Where they legitimately differ today, the rule is written so that the
//! difference is visible rather than fatal:
//!
//! * A route a build cannot serve answers `503 unavailable`
//!   ([`ErrorCode::Unavailable`](screeny_device_api::ErrorCode::Unavailable)),
//!   which several rules accept explicitly - `GET /api/v1/networks` and
//!   `POST /api/v1/firmware` do that on firmware 0.4.0 until cards 223 and
//!   240. **The suite needs no way to tell a simulator from a device**: the
//!   API's own "I cannot do that" is the answer it reads.
//! * The two rules that firmware 0.4.0 is known to get wrong - `wifi_state`
//!   carrying the sticky trial result into `GET /api/v1/status`, and
//!   `GET /api/v1/wifi`'s `reason` reading `null` after a failure - are marked
//!   [`KNOWN_223`]. While [`CARD_223_LANDED`] is `false` a *failure* of one of
//!   those rules is reported as `SKIP` with the card number; a pass is still a
//!   pass, so the simulator is held to them today. **Flip that one constant
//!   when card 223 lands and both become enforced everywhere.**

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use screeny_device_api::reply::{SettingsReply, StatusReply};
use screeny_device_api::{route, IdleMode};
use screeny_proto::control::{Request, Telemetry};

use crate::link::{Control, OwnedReply};

pub mod client;
mod rules;

pub use client::{Client, Res};

/// Whether card 223 has landed on the device.
///
/// Firmware 0.4.0 reports the sticky result of the last credentials attempt in
/// `GET /api/v1/status`'s `wifi_state`, and answers `null` for
/// `GET /api/v1/wifi`'s `reason` after a failure. `docs/design/device-web.md`
/// says both are wrong and assigns them to card 223.
///
/// **This is the one constant to flip when 223 lands**: the [`KNOWN_223`]
/// rules stop being downgraded to `SKIP` and are enforced against every
/// target.
pub const CARD_223_LANDED: bool = false;

// ---------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------

/// Rules that reboot the device. `--allow-reboot` opts in.
pub const ALLOW_REBOOT: u8 = 1 << 0;
/// Rules that post credentials and wait out the fallback, which takes the
/// device off its network for about a minute. `--allow-wifi-trial` opts in.
pub const ALLOW_WIFI_TRIAL: u8 = 1 << 1;
/// Rules that drive the panel to the firmware's brightness cap, which may be
/// brighter than what the suite found. `--cap-probe` opts in, exactly as in
/// the UDP suite.
pub const CAP_PROBE: u8 = 1 << 2;
/// Rules that need the UDP control port as well as HTTP. Skipped when nothing
/// answered there at startup.
pub const NEEDS_UDP: u8 = 1 << 3;
/// Rules firmware 0.4.0 is known to fail, fixed by card 223. See
/// [`CARD_223_LANDED`].
pub const KNOWN_223: u8 = 1 << 4;

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

/// What a rule decided.
pub enum Outcome {
    /// It held, and here is the measurement.
    Pass(String),
    /// It did not hold, and here is what came back instead.
    Fail(String),
    /// It could not be asked, and here is why.
    Skip(String),
}

/// The shape every rule ends with: a verdict and the measurement behind it.
pub fn verdict(ok: bool, detail: impl Into<String>) -> Result<Outcome, String> {
    let d = detail.into();
    Ok(if ok { Outcome::Pass(d) } else { Outcome::Fail(d) })
}

/// One rule.
pub struct Rule {
    /// Its number, stable across runs, and what `--only N` takes.
    pub n: u16,
    /// The group it belongs to, and what `--only SECTION` takes.
    pub section: &'static str,
    /// The route it is about, as `crates/device-api` names it, or `-` for the
    /// rules that are about the whole run.
    pub route: &'static str,
    /// What it checks, in one line.
    pub name: &'static str,
    /// Where it comes from: a `device-web.md` section or a `crates/device-api`
    /// item.
    pub cite: &'static str,
    /// Roughly how long it takes, for the estimate printed up front.
    pub secs: f32,
    /// [`ALLOW_REBOOT`] and friends.
    pub flags: u8,
    /// The check itself.
    pub run: fn(&mut Ctx) -> Result<Outcome, String>,
}

/// Every rule, in the order they run.
#[must_use]
pub fn all() -> Vec<Rule> {
    rules::all()
}

// ---------------------------------------------------------------------------
// What a rule is handed
// ---------------------------------------------------------------------------

/// One response the suite saw, kept for the rules that are about the whole
/// run: the size bounds, the error shape and the PSK grep.
#[derive(Debug, Clone)]
pub struct Seen {
    /// The method sent.
    pub method: String,
    /// The path asked for.
    pub path: String,
    /// The status that came back.
    pub status: u16,
    /// The `Content-Type`, without parameters.
    pub content_type: String,
    /// The body.
    pub body: Vec<u8>,
    /// How long it took, connect included.
    pub elapsed: Duration,
}

/// The settings the suite found, and puts back.
#[derive(Debug, Clone)]
pub struct Found {
    /// The friendly name. Empty means `screeny-<id>`.
    pub name: String,
    /// The brightness in effect. Nothing here ever asks for more than this.
    pub brightness: u8,
    /// The idle mode in effect.
    pub idle_mode: IdleMode,
    /// The firmware version, for the header line.
    pub fw: String,
    /// The device id, for the header line.
    pub id: String,
}

/// What a rule is handed.
pub struct Ctx {
    /// The HTTP client. Rules normally go through [`Ctx::get`] and friends so
    /// that the reply is recorded; this is here for the rules that need to
    /// send something malformed.
    pub http: Client,
    /// The UDP control port, when one answered at startup.
    pub ctrl: Option<Control>,
    /// What the suite found, and will restore.
    pub found: Found,
    /// Whether `--cap-probe` was given.
    pub cap_probe: bool,
    /// Every response the suite has seen so far.
    pub seen: Vec<Seen>,
}

impl Ctx {
    /// `GET path`, recorded.
    ///
    /// # Errors
    /// Anything that stops a response coming back.
    pub fn get(&mut self, path: &str) -> Result<Res, String> {
        self.record("GET", path, self.http.get(path))
    }

    /// `POST path` with a JSON body, recorded.
    ///
    /// # Errors
    /// Anything that stops a response coming back.
    pub fn post_json(&mut self, path: &str, body: &str) -> Result<Res, String> {
        self.record("POST", path, self.http.post_json(path, body))
    }

    /// `POST path` with an urlencoded body, recorded.
    ///
    /// # Errors
    /// Anything that stops a response coming back.
    pub fn post_form(&mut self, path: &str, body: &str) -> Result<Res, String> {
        self.record("POST", path, self.http.post_form(path, body))
    }

    /// `POST path` with a raw octet-stream body, recorded.
    ///
    /// # Errors
    /// Anything that stops a response coming back.
    pub fn post_bytes(&mut self, path: &str, body: &[u8]) -> Result<Res, String> {
        self.record("POST", path, self.http.post_bytes(path, body))
    }

    /// Any method at all, recorded. For the verbs this API has no
    /// [`Method`](screeny_device_api::Method) for.
    ///
    /// # Errors
    /// Anything that stops a response coming back.
    pub fn request(
        &mut self,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        body: &[u8],
    ) -> Result<Res, String> {
        self.record(method, path, self.http.request(method, path, content_type, body))
    }

    fn record(&mut self, method: &str, path: &str, r: Result<Res, String>) -> Result<Res, String> {
        let res = r?;
        self.seen.push(Seen {
            method: method.to_string(),
            path: path.to_string(),
            status: res.status,
            content_type: res.content_type(),
            body: res.body.clone(),
            elapsed: res.elapsed,
        });
        Ok(res)
    }

    /// `GET /api/v1/status`, parsed.
    ///
    /// # Errors
    /// A non-200, or a body that is not a [`StatusReply`].
    pub fn status(&mut self) -> Result<StatusReply, String> {
        let res = self.get(route::STATUS)?;
        if res.status != 200 {
            return Err(format!(
                "GET {} answered {}: {}",
                route::STATUS,
                res.status,
                res.snippet(80)
            ));
        }
        res.parse()
    }

    /// One UDP `TELEMETRY` request, when there is a control port.
    ///
    /// # Errors
    /// Whatever the control link says.
    pub fn telemetry(&mut self) -> Result<Telemetry, String> {
        let ctrl = self
            .ctrl
            .as_mut()
            .ok_or("no UDP control port for this target")?;
        match ctrl.request(Request::Telemetry)? {
            OwnedReply::Telemetry(t) => Ok(t),
            other => Err(format!("expected telemetry, got {other:?}")),
        }
    }

    /// Poll `GET /api/v1/status` until the device answers, for the rules that
    /// take it off the network or restart it.
    ///
    /// Connection failures are expected while it is away and are not reported
    /// as failures; running out of patience is.
    ///
    /// # Errors
    /// When the deadline passes with nothing answering.
    pub fn wait_for_device(&mut self, patience: Duration) -> Result<StatusReply, String> {
        let t0 = Instant::now();
        let mut last = String::new();
        while t0.elapsed() < patience {
            match self.status() {
                Ok(s) => return Ok(s),
                Err(e) => last = e,
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        Err(format!(
            "nothing answered in {:.0} s (last: {last})",
            patience.as_secs_f32()
        ))
    }

    /// Put one settings value back, for a rule that changed it.
    ///
    /// # Errors
    /// Anything that stops the POST succeeding.
    pub fn set_settings(&mut self, body: &str) -> Result<SettingsReply, String> {
        let res = self.post_json(route::SETTINGS, body)?;
        if res.status != 200 {
            return Err(format!(
                "POST {} {body} answered {}: {}",
                route::SETTINGS,
                res.status,
                res.snippet(80)
            ));
        }
        res.parse()
    }
}

/// The JSON name of an [`IdleMode`], without the quotes.
#[must_use]
pub fn idle_name(m: IdleMode) -> &'static str {
    match m {
        IdleMode::Status => "status",
        IdleMode::HoldForever => "hold_forever",
        IdleMode::Dim => "dim",
        IdleMode::Black => "black",
    }
}

// ---------------------------------------------------------------------------
// Restoring what the suite touched
// ---------------------------------------------------------------------------

/// Everything the suite changes, and how to put it back.
///
/// [`RestoreGuard`] covers a normal return and a panic; the ctrl-c handler
/// installed by [`run`] covers the third case. Both go through
/// [`Restore::apply`], which opens its own connection because the handler runs
/// on another thread.
#[derive(Debug, Clone)]
pub struct Restore {
    /// Where to send the settings.
    pub client: Client,
    /// The name found at startup.
    pub name: String,
    /// The brightness found at startup.
    pub brightness: u8,
    /// The idle mode found at startup.
    pub idle_mode: IdleMode,
}

impl Restore {
    /// Stop any identify overlay and put the three settings back.
    ///
    /// # Errors
    /// Anything that stops either POST succeeding.
    pub fn apply(&self) -> Result<SettingsReply, String> {
        // The overlay first: it is the one thing a person watching the panel
        // would notice, and it costs one request whether or not it is up.
        let _ = self
            .client
            .post_json(route::IDENTIFY, r#"{"duration_ms":0}"#);
        let body = format!(
            r#"{{"name":{},"brightness":{},"idle_mode":"{}"}}"#,
            json_string(&self.name),
            self.brightness,
            idle_name(self.idle_mode)
        );
        let res = self.client.post_json(route::SETTINGS, &body)?;
        if res.status != 200 {
            return Err(format!("settings answered {}: {}", res.status, res.snippet(80)));
        }
        res.parse()
    }
}

/// A JSON string literal. Names are at most 32 bytes and come from the device,
/// but a device that answered with a quote in its name must not produce a body
/// this suite cannot send.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Applies a [`Restore`] when it goes out of scope, however it goes out of
/// scope. Disarmed once the runner has applied it explicitly and reported what
/// came back.
pub struct RestoreGuard {
    /// What to put back.
    pub inner: Restore,
    /// Whether `Drop` should still do it.
    pub armed: bool,
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.inner.apply();
        }
    }
}

// ---------------------------------------------------------------------------
// The runner
// ---------------------------------------------------------------------------

/// How to run the suite.
#[derive(Debug, Clone)]
pub struct Opts {
    /// Where the HTTP server is.
    pub http_addr: SocketAddr,
    /// What to put in the `Host` header.
    pub host: String,
    /// The UDP control port, when this target has one. The rules that compare
    /// HTTP with the wire are skipped without it.
    pub ctrl_addr: Option<SocketAddr>,
    /// Run only the rule with this number, or only the rules whose section
    /// starts with this.
    pub only: Option<String>,
    /// Allow the rules that reboot the device.
    pub allow_reboot: bool,
    /// Allow the rules that post credentials and wait out the fallback.
    pub allow_wifi_trial: bool,
    /// Allow the rule that drives brightness to the firmware cap.
    pub cap_probe: bool,
    /// Install a ctrl-c handler that restores the device. Off inside tests,
    /// where the handler is process-wide and the `Drop` guard is enough.
    pub ctrlc: bool,
}

impl Opts {
    /// The defaults: every opt-in off, every rule that is safe.
    #[must_use]
    pub fn new(http_addr: SocketAddr, host: impl Into<String>) -> Self {
        Opts {
            http_addr,
            host: host.into(),
            ctrl_addr: None,
            only: None,
            allow_reboot: false,
            allow_wifi_trial: false,
            cap_probe: false,
            ctrlc: true,
        }
    }
}

/// What the run added up to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// Rules that held.
    pub passed: usize,
    /// Rules that did not.
    pub failed: usize,
    /// Rules that could not be asked.
    pub skipped: usize,
}

impl Summary {
    /// True when nothing failed.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.failed == 0
    }
}

/// Print what the suite is about to do, run it, and put the device back.
///
/// # Errors
/// When `--only` matches nothing, or when the target does not answer
/// `GET /api/v1/status` at all - which is reported once, rather than as forty
/// failures.
#[allow(clippy::too_many_lines)]
pub fn run(opts: &Opts) -> Result<Summary, String> {
    let rules: Vec<Rule> = all().into_iter().filter(|r| selected(r, opts)).collect();
    if rules.is_empty() {
        return Err(format!(
            "no rules match --only {:?}",
            opts.only.clone().unwrap_or_default()
        ));
    }

    let http = Client::new(opts.http_addr, opts.host.clone());
    // One round trip before anything else, so "the device is not there" is a
    // clear error rather than forty failures.
    let first = http
        .get(route::STATUS)
        .map_err(|e| format!("GET http://{}{}: {e}", opts.host, route::STATUS))?;
    if first.status != 200 {
        return Err(format!(
            "GET {} answered {}: {}",
            route::STATUS,
            first.status,
            first.snippet(120)
        ));
    }
    let found: StatusReply = first.parse()?;
    let found = Found {
        name: found.name.as_str().to_string(),
        brightness: found.brightness,
        idle_mode: found.idle_mode,
        fw: found.fw.as_str().to_string(),
        id: found.id.as_str().to_string(),
    };

    // The UDP half, if this target has one. A device always does; a caller
    // pointed at somebody else's HTTP server may not.
    let ctrl = opts.ctrl_addr.and_then(|addr| {
        let mut c = Control::connect(addr).ok()?;
        match c.request(Request::Telemetry) {
            Ok(OwnedReply::Telemetry(_)) => Some(c),
            _ => None,
        }
    });

    let will_run = |r: &Rule| {
        (r.flags & ALLOW_REBOOT == 0 || opts.allow_reboot)
            && (r.flags & ALLOW_WIFI_TRIAL == 0 || opts.allow_wifi_trial)
            && (r.flags & CAP_PROBE == 0 || opts.cap_probe)
            && (r.flags & NEEDS_UDP == 0 || ctrl.is_some())
    };
    let estimate: f32 = rules.iter().filter(|r| will_run(r)).map(|r| r.secs).sum();

    println!(
        "screeny-probe http: {} rules against {} ({})",
        rules.len(),
        base_url(opts),
        if opts.http_addr.ip().is_loopback() {
            "loopback"
        } else {
            "remote"
        }
    );
    println!(
        "  fw {} id {}, name {:?}, brightness {}, idle {}; estimated {:.0} s",
        found.fw,
        found.id,
        found.name,
        found.brightness,
        idle_name(found.idle_mode),
        estimate
    );
    match &ctrl {
        Some(c) => println!("  UDP control at {} answers", c.addr()),
        None => println!("  no UDP control port: the rules that compare the two are skipped"),
    }
    if opts.allow_wifi_trial {
        println!(
            "  --allow-wifi-trial: this posts the dummy pair Example-Wifi1 / password9 and\n\
                 waits for the fallback. THE DEVICE IS OFF ITS NETWORK FOR ABOUT A MINUTE."
        );
    }
    if opts.allow_reboot {
        println!("  --allow-reboot: this restarts the device and waits for it to come back.");
    }
    if opts.cap_probe {
        println!("  --cap-probe: this drives brightness to the firmware cap.");
    }
    if !opts.allow_reboot || !opts.allow_wifi_trial {
        println!("  (--allow-reboot and --allow-wifi-trial add the rules that are not safe by default)");
    }
    println!();

    // Armed from here on: every exit path puts the device back.
    let restore = Restore {
        client: http.clone(),
        name: found.name.clone(),
        brightness: found.brightness,
        idle_mode: found.idle_mode,
    };
    let mut guard = RestoreGuard {
        inner: restore.clone(),
        armed: true,
    };
    if opts.ctrlc {
        let on_interrupt = restore;
        if let Err(e) = ctrlc::set_handler(move || {
            eprintln!("\ninterrupted: restoring the device");
            match on_interrupt.apply() {
                Ok(s) => eprintln!("  name {:?} brightness {} restored", s.name.as_str(), s.brightness),
                Err(e) => eprintln!("  RESTORE FAILED: {e}"),
            }
            std::process::exit(130);
        }) {
            eprintln!("warning: no ctrl-c handler ({e}); ctrl-c will not restore");
        }
    }

    let mut cx = Ctx {
        http,
        ctrl,
        found,
        cap_probe: opts.cap_probe,
        seen: Vec::new(),
    };

    let mut s = Summary {
        passed: 0,
        failed: 0,
        skipped: 0,
    };
    let total = rules.len();
    for (i, r) in rules.iter().enumerate() {
        let outcome = if r.flags & ALLOW_REBOOT != 0 && !opts.allow_reboot {
            Outcome::Skip("needs --allow-reboot (restarts the device)".into())
        } else if r.flags & ALLOW_WIFI_TRIAL != 0 && !opts.allow_wifi_trial {
            Outcome::Skip("needs --allow-wifi-trial (a minute off the network)".into())
        } else if r.flags & CAP_PROBE != 0 && !opts.cap_probe {
            Outcome::Skip("needs --cap-probe (would raise brightness to the cap)".into())
        } else if r.flags & NEEDS_UDP != 0 && cx.ctrl.is_none() {
            Outcome::Skip("needs the UDP control port, which did not answer".into())
        } else {
            match (r.run)(&mut cx) {
                Ok(o) => o,
                Err(e) => Outcome::Fail(format!("error: {e}")),
            }
        };
        // A rule firmware 0.4.0 is known to fail is reported as a known
        // difference rather than as a failure, until card 223 lands. A *pass*
        // is still a pass, so the simulator is held to it today.
        let outcome = match outcome {
            Outcome::Fail(why) if r.flags & KNOWN_223 != 0 && !CARD_223_LANDED => Outcome::Skip(
                format!("known difference, card 223 (flip CARD_223_LANDED): {why}"),
            ),
            other => other,
        };
        let (tag, detail) = match &outcome {
            Outcome::Pass(d) => ("PASS", d),
            Outcome::Fail(d) => ("FAIL", d),
            Outcome::Skip(d) => ("SKIP", d),
        };
        match outcome {
            Outcome::Pass(_) => s.passed += 1,
            Outcome::Fail(_) => s.failed += 1,
            Outcome::Skip(_) => s.skipped += 1,
        }
        println!(
            "[{:>2}/{}] #{:02} {:<9} {:<58} {tag}  {detail}",
            i + 1,
            total,
            r.n,
            r.section,
            r.name
        );
    }

    println!();
    // Take the restore off the Drop path so its result can be reported.
    guard.armed = false;
    match guard.inner.apply() {
        Ok(v) => println!(
            "restored: name {:?}, brightness {} (found {}), idle mode {}, identify off",
            v.name.as_str(),
            v.brightness,
            cx.found.brightness,
            idle_name(v.idle_mode)
        ),
        Err(e) => {
            println!("RESTORE FAILED: {e}");
            s.failed += 1;
        }
    }
    println!(
        "http conformance: {} passed, {} failed, {} skipped",
        s.passed, s.failed, s.skipped
    );
    Ok(s)
}

/// The URL the suite is pointed at, as a person would type it: the `Host`
/// header's value, which already carries a port when it has one.
fn base_url(opts: &Opts) -> String {
    if opts.host.contains(':') || opts.http_addr.port() == 80 {
        format!("http://{}", opts.host)
    } else {
        format!("http://{}:{}", opts.host, opts.http_addr.port())
    }
}

/// Whether `--only` selects this rule: by number, or by section prefix.
fn selected(r: &Rule, opts: &Opts) -> bool {
    match &opts.only {
        None => true,
        Some(p) => match p.parse::<u16>() {
            Ok(n) => r.n == n,
            Err(_) => r.section.starts_with(p.as_str()) || r.route.contains(p.as_str()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_numbers_are_unique_and_in_order() {
        let rules = all();
        assert!(rules.len() >= 30, "the catalogue has shrunk");
        for w in rules.windows(2) {
            assert!(
                w[0].n < w[1].n,
                "rule {} comes after {}",
                w[1].n,
                w[0].n
            );
        }
    }

    /// The same guard `crates/sim/tests/http_routes.rs` puts on the server: a
    /// row added to `ROUTES` and not checked here is a failure with the
    /// route's name in it, not a gap nobody notices.
    #[test]
    fn every_route_in_the_table_has_a_rule() {
        let rules = all();
        for r in route::ROUTES {
            assert!(
                rules.iter().any(|rule| rule.route == r.path),
                "card 228: {:?} {} is in screeny_device_api::route::ROUTES and no rule \
                 in this suite mentions it",
                r.method,
                r.path
            );
        }
    }

    #[test]
    fn every_rule_says_where_it_comes_from() {
        for r in all() {
            assert!(!r.cite.is_empty(), "rule {} has no citation", r.n);
            assert!(!r.name.is_empty(), "rule {} has no name", r.n);
            assert!(r.secs > 0.0, "rule {} has no estimate", r.n);
        }
    }

    #[test]
    fn only_takes_a_number_or_a_section() {
        let rules = all();
        let one = |p: &str| {
            let mut o = Opts::new("127.0.0.1:80".parse().unwrap(), "x");
            o.only = Some(p.into());
            rules.iter().filter(|r| selected(r, &o)).count()
        };
        assert_eq!(one("1"), 1, "a number is one rule");
        assert!(one("status") >= 3, "a section is several");
        // A path fragment works too, which is how `--only /api/v1/wifi` reads.
        assert!(one("/api/v1/wifi") >= 3, "a route is several");
        assert_eq!(one("zzz"), 0);
    }

    #[test]
    fn a_name_with_a_quote_in_it_still_makes_a_body_the_device_can_parse() {
        assert_eq!(json_string("desk"), "\"desk\"");
        assert_eq!(json_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_string("a\u{1}b"), "\"a\\u0001b\"");
    }
}
