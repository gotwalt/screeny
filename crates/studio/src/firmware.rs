//! Card 364: firmware updates, from the page and from Home Assistant.
//!
//! The Studio ships with one firmware image - the app image
//! (`screeny-fw-<version>.bin`, not the `-full.bin` a first install flashes) -
//! put there by the Dockerfile at build time. [`Firmware::load`] reads it with
//! the device's own validator ([`screeny_fwimage::Scan`]) and takes its version
//! from `esp_app_desc`. **No file means no update is offered** and nothing else
//! changes; there is no internet at runtime.
//!
//! An update is only ever started by an explicit action (the page's button or
//! Home Assistant's Install): [`start`]. It then
//!
//! 1. uploads the image to the panel (`POST /api/v1/firmware`, which activates;
//!    [`crate::devhttp::post_firmware`]). The Studio's frame stream is not
//!    touched: the firmware's upload keeps the stream alive (card 240) and puts
//!    its "updating" screen up itself;
//! 2. waits for the panel to **go away and come back with a different `boot_id`**
//!    (the old image keeps answering for about two seconds after the reply, and
//!    a poll in that window must not be mistaken for success, card 246);
//! 3. waits for the new image to confirm itself: it is on trial
//!    (`fw_state` `pending_verify`) until it has proved itself healthy, and the
//!    bootloader rolls it back if it does not. The panel coming back on the
//!    **old** version is that rollback, and is reported as a failure.
//!
//! While a run is active the status poller leaves that panel alone ([`Firmware::active`]):
//! the device has one connection worker and no listen backlog, and a second
//! caller during an upload is dropped at SYN.
//!
//! A panel whose firmware is **newer than or equal to** what is offered is
//! offered nothing: this never downgrades.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use screeny_device_api::{FirmwareError, FwState};

use crate::devhttp;
use crate::AppState;

/// The largest image taken: an OTA slot is 2 MB, and the image is under half.
const MAX_IMAGE: usize = 0x20_0000;

/// Where the Dockerfile puts the image. [`crate::Config::firmware`] is `None`
/// by default and the binary reads this when the environment says nothing.
pub const DEFAULT_PATH: &str = "/usr/local/share/screeny/screeny-fw.bin";

/// How patient each stage of an update is.
///
/// The defaults are the device's own numbers with room to spare (the same
/// ones `screeny-probe fw-upload --activate` uses): a boot to a DHCP address is
/// ~20 s, a trial confirms between 60 and 120 s and reverts at 180 s. A test
/// sets its own, in tenths of a second.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// How long the whole upload may take.
    pub upload: Duration,
    /// How long to wait, after the reply, for a different `boot_id` to answer.
    pub reappear: Duration,
    /// How long to wait after that for the trial to end.
    pub decide: Duration,
    /// How long between polls.
    pub poll: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            upload: devhttp::UPLOAD_TIMEOUT,
            reappear: Duration::from_secs(120),
            decide: Duration::from_secs(240),
            poll: Duration::from_secs(2),
        }
    }
}

/// The image this Studio offers.
#[derive(Clone)]
pub struct Offer {
    /// `esp_app_desc.version`, e.g. `0.11.0`.
    pub version: String,
    /// The whole image, as it goes to the panel.
    pub bytes: Arc<[u8]>,
}

impl std::fmt::Debug for Offer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Offer").field("version", &self.version).field("bytes", &self.bytes.len()).finish()
    }
}

/// Read and check an image the way the panel will.
///
/// # Errors
///
/// A sentence: unreadable, too big, not a `screeny-fw` image, or a version
/// that is not printable.
pub fn check_image(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() > MAX_IMAGE {
        return Err(format!("{} bytes is more than an OTA slot", bytes.len()));
    }
    let mut scan = screeny_fwimage::Scan::new(u32::try_from(MAX_IMAGE).unwrap_or(u32::MAX));
    for chunk in bytes.chunks(screeny_fwimage::SECTOR) {
        scan.push(chunk).map_err(|e| format!("not an image this panel would take: {e:?}"))?;
        scan.check_front().map_err(|e| format!("not an image this panel would take: {e:?}"))?;
    }
    let image = scan.finish().map_err(|e| format!("not an image this panel would take: {e:?}"))?;
    image.version.as_str().map(str::to_string).ok_or_else(|| "its version is not printable".to_string())
}

/// `(major, minor, patch)` of `1.2.3`, `1.2.3-rc1` or `v1.2.3+abc`: the leading
/// numeric dotted part. A pre-release suffix is ignored, so `0.11.0-rc1` and
/// `0.11.0` are the same version here.
#[must_use]
pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v');
    let core = core.split(['-', '+', ' ']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    Some((major, minor, patch))
}

/// Is `offered` an update for a panel running `installed`? **Only if strictly
/// newer**: the same or newer on the panel is no update and never a downgrade,
/// and a version either side that cannot be read is no update either.
#[must_use]
pub fn is_update(installed: &str, offered: &str) -> bool {
    match (parse_version(installed), parse_version(offered)) {
        (Some(i), Some(o)) => o > i,
        _ => false,
    }
}

/// Where one panel's update has got to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Run {
    /// The image is going to the panel.
    Uploading { sent: usize, total: usize },
    /// Accepted; the panel is restarting into it.
    Restarting,
    /// It is back on the new image, on trial.
    Trial,
    /// The last attempt did not work, and why.
    Failed { why: String },
}

impl Run {
    /// Is this attempt still going?
    #[must_use]
    pub fn is_active(&self) -> bool {
        !matches!(self, Run::Failed { .. })
    }

    /// 0-100 while the image is going out; `None` when there is no honest
    /// figure (waiting for a reboot has no length).
    #[must_use]
    pub fn percent(&self) -> Option<u8> {
        match self {
            Run::Uploading { sent, total } if *total > 0 => Some(u8::try_from((sent * 100 / total).min(100)).unwrap_or(100)),
            _ => None,
        }
    }
}

/// What the page and Home Assistant are told about one panel's firmware.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct View {
    /// What the panel runs, if it has said.
    pub installed: Option<String>,
    /// The version this Studio carries; `None` when it carries none.
    pub offered: Option<String>,
    /// There is an update to install **and** it can be started now.
    pub available: bool,
    /// Why not, when there is something to say: nothing offered, already up to
    /// date, the panel not answering, on trial.
    pub why_not: Option<String>,
    /// The attempt in progress, or the last one that failed.
    pub run: Option<Run>,
}

/// The offer and every panel's attempt.
#[derive(Debug)]
pub struct Firmware {
    offer: Option<Offer>,
    timing: Timing,
    runs: Mutex<BTreeMap<String, Run>>,
}

impl Firmware {
    /// Load the offer from `path`, if there is one. Never fatal: a Studio with
    /// no image, or a bad one, offers nothing and says so once.
    ///
    /// Nothing at [`DEFAULT_PATH`] is silent: it is the ordinary case on a
    /// laptop, where there is no image. Nothing at a path somebody named is not.
    #[must_use]
    pub fn load(path: Option<&Path>, timing: Timing) -> Firmware {
        let offer = path.and_then(|p| match std::fs::read(p) {
            Ok(bytes) => match check_image(&bytes) {
                Ok(version) => {
                    eprintln!("studio: offers firmware {version} to panels ({})", p.display());
                    Some(Offer { version, bytes: bytes.into() })
                }
                Err(e) => {
                    eprintln!("studio: {}: {e}; no firmware is offered", p.display());
                    None
                }
            },
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound || p != Path::new(DEFAULT_PATH) {
                    eprintln!("studio: {}: {e}; no firmware is offered", p.display());
                }
                None
            }
        });
        Firmware { offer, timing, runs: Mutex::new(BTreeMap::new()) }
    }

    /// An offer made directly (tests).
    #[must_use]
    pub fn with_offer(offer: Option<Offer>, timing: Timing) -> Firmware {
        Firmware { offer, timing, runs: Mutex::new(BTreeMap::new()) }
    }

    /// The version on offer.
    #[must_use]
    pub fn offered(&self) -> Option<&str> {
        self.offer.as_ref().map(|o| o.version.as_str())
    }

    fn runs(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Run>> {
        self.runs.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Is an update to this device going on right now? The status poller asks:
    /// it must not open a second connection to a panel that is being written.
    #[must_use]
    pub fn active(&self, device: &str) -> bool {
        self.runs().get(device).is_some_and(Run::is_active)
    }

    /// The attempt in progress, or the last one that failed.
    #[must_use]
    pub fn run(&self, device: &str) -> Option<Run> {
        self.runs().get(device).cloned()
    }

    fn set(&self, device: &str, run: Run) {
        self.runs().insert(device.to_string(), run);
    }

    fn clear(&self, device: &str) {
        self.runs().remove(device);
    }

    /// The firmware story of one device: what it runs, what is on offer, and
    /// whether Install would do anything.
    #[must_use]
    pub fn view(&self, device: &str, installed: Option<&str>, state: Option<FwState>) -> View {
        let run = self.run(device);
        let offered = self.offered().map(str::to_string);
        let installed = installed.map(str::to_string);
        let why_not = match (&offered, &installed) {
            _ if run.as_ref().is_some_and(Run::is_active) => Some("an update is in progress".to_string()),
            (None, _) => Some("this Studio carries no firmware to offer".to_string()),
            (Some(_), None) => Some("the panel has not said what firmware it runs".to_string()),
            (Some(o), Some(i)) if !is_update(i, o) => Some(if parse_version(i).is_some() && parse_version(o).is_some() {
                "the panel is up to date".to_string()
            } else {
                "the versions cannot be compared".to_string()
            }),
            _ if state.is_some_and(|s| s != FwState::Valid) => Some("the panel is still proving the firmware it is running".to_string()),
            _ => None,
        };
        View { available: why_not.is_none(), why_not, installed, offered, run }
    }
}

/// Start updating `device`. Returns at once; the work is a task, and its
/// progress is [`Firmware::run`].
///
/// # Errors
///
/// A sentence for the page or the log: nothing to offer, no such panel, not
/// reachable, nothing newer, or already going.
pub fn start(st: &AppState, device: &str) -> Result<(), String> {
    let fw = Arc::clone(&st.firmware);
    let offer = fw.offer.clone().ok_or("this Studio carries no firmware to offer")?;
    let record = st.devices.get(device).ok_or_else(|| format!("no panel `{device}`"))?;
    let facts = record.facts.as_ref().ok_or("the panel has not answered its status API yet, so its firmware is unknown")?;
    let installed = facts.reply.fw.to_string();
    let addr = record.http_addr(st.cfg.device_http_port).ok_or("the panel's address is not known yet")?;
    let view = fw.view(device, Some(&installed), Some(facts.reply.fw_state));
    if !view.available {
        return Err(view.why_not.unwrap_or_else(|| "nothing to install".to_string()));
    }
    // Claimed under the lock, so two Installs cannot both start.
    {
        let mut runs = fw.runs();
        if runs.get(device).is_some_and(Run::is_active) {
            return Err("an update is already in progress".to_string());
        }
        runs.insert(device.to_string(), Run::Uploading { sent: 0, total: offer.bytes.len() });
    }
    eprintln!("studio: updating `{}` from firmware {installed} to {}", record.label(), offer.version);
    let st = st.clone();
    let device = device.to_string();
    let before = facts.reply.boot_id;
    tokio::spawn(async move {
        match run_update(&st, &device, addr, &offer, before, &installed).await {
            Ok(()) => {
                eprintln!("studio: `{device}` is running firmware {} now", offer.version);
                st.firmware.clear(&device);
            }
            Err(why) => {
                eprintln!("studio: updating `{device}` to {} failed: {why}", offer.version);
                st.firmware.set(&device, Run::Failed { why });
            }
        }
        st.changed(None);
    });
    Ok(())
}

async fn run_update(st: &AppState, device: &str, addr: std::net::SocketAddr, offer: &Offer, before: u32, installed: &str) -> Result<(), String> {
    let fw = &st.firmware;
    let timing = fw.timing;
    let total = offer.bytes.len();

    // The panel is about to reboot on purpose: not a crash, not "unasked".
    st.devices.asked_to_reboot(device);

    // --- 1: the upload ------------------------------------------------------
    let bytes = Arc::clone(&offer.bytes);
    let progress_to = Arc::clone(&st.firmware);
    let id = device.to_string();
    let reply = tokio::task::spawn_blocking(move || {
        devhttp::post_firmware(addr, &bytes, true, timing.upload, &mut |sent| progress_to.set(&id, Run::Uploading { sent, total }))
    })
    .await
    .map_err(|e| format!("the upload task failed: {e}"))?
    .map_err(|f| f.to_string())?;
    if !reply.ok {
        return Err(match reply.error {
            Some(e) => format!("the panel refused the image: {}", explain(e)),
            None => "the panel refused the image".to_string(),
        });
    }
    if !reply.activating {
        return Err("the panel took the image but would not switch to it".to_string());
    }
    fw.set(device, Run::Restarting);

    // --- 2: wait for a boot_id that is not the one we started with ----------
    let started = Instant::now();
    let mut went_away = false;
    let back = loop {
        if started.elapsed() > timing.reappear {
            return Err(if went_away {
                format!("the panel did not come back within {} s", timing.reappear.as_secs())
            } else {
                "the panel accepted the image but never restarted".to_string()
            });
        }
        tokio::time::sleep(timing.poll).await;
        match status(addr).await {
            Some(s) if s.boot_id == before => {} // the old image, not gone yet
            Some(s) => break s,
            None => went_away = true,
        }
    };

    // --- 3: wait for the trial to end ---------------------------------------
    let came_back = Instant::now();
    let mut now = back;
    loop {
        let running = now.fw.to_string();
        let state = now.fw_state;
        st.devices.heard_http(device, now.clone());
        if running != offer.version {
            return Err(format!("the panel came back on firmware {running}, not {}: it rolled the update back", offer.version));
        }
        if state == FwState::Valid {
            return Ok(());
        }
        if matches!(state, FwState::Invalid | FwState::Aborted) {
            return Err(format!("the new firmware did not pass its trial; the panel is back on {installed}"));
        }
        fw.set(device, Run::Trial);
        loop {
            if came_back.elapsed() > timing.decide {
                return Err(format!("the new firmware was still on trial after {} s", timing.decide.as_secs()));
            }
            tokio::time::sleep(timing.poll).await;
            if let Some(s) = status(addr).await {
                now = s;
                break;
            }
        }
    }
}

async fn status(addr: std::net::SocketAddr) -> Option<screeny_device_api::reply::StatusReply> {
    tokio::task::spawn_blocking(move || devhttp::get_status(addr, devhttp::TIMEOUT)).await.ok()?.ok()
}

/// The panel's refusal, in words.
fn explain(e: FirmwareError) -> &'static str {
    match e {
        FirmwareError::BadMagic => "not an ESP32 image",
        FirmwareError::WrongChip => "built for a different chip",
        FirmwareError::WrongProject => "not screeny firmware",
        FirmwareError::BadChecksum => "the image's checksum is wrong",
        FirmwareError::BadSha256 => "the image's hash does not match",
        FirmwareError::TooLarge => "too big for the update slot",
        FirmwareError::Busy => "another update or flash write is going on",
        FirmwareError::Flash => "the flash write failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_compared_numerically_and_never_downgraded() {
        assert!(is_update("0.10.0", "0.11.0"));
        assert!(is_update("0.9.0", "0.10.0"), "numerically, not as text");
        assert!(!is_update("0.11.0", "0.11.0"), "equal is not an update");
        assert!(!is_update("0.12.0", "0.11.0"), "never a downgrade");
        assert!(!is_update("1.0.0", "0.99.9"));
        assert!(is_update("0.10.0", "0.10.1"));
        assert!(!is_update("0.10.0-dirty", "0.10.0"), "a suffix does not make it older");
        assert!(!is_update("garbage", "0.11.0"), "unreadable is no update");
        assert!(!is_update("0.10.0", "garbage"));
        assert_eq!(parse_version("v1.2.3+abc"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn an_image_is_checked_the_way_the_panel_checks_it() {
        let good = screeny_fwimage::build::Builder { version: "0.11.0", ..screeny_fwimage::build::Builder::good() }.build();
        assert_eq!(check_image(&good).as_deref(), Ok("0.11.0"));
        let wrong = screeny_fwimage::build::Builder::wrong_project().build();
        assert!(check_image(&wrong).is_err());
        assert!(check_image(b"not firmware").is_err());
        assert!(check_image(&[]).is_err());
    }

    #[test]
    fn a_view_says_why_there_is_nothing_to_install() {
        let offer = |v: &str| Offer { version: v.into(), bytes: Arc::from(vec![0u8; 4]) };
        let none = Firmware::with_offer(None, Timing::default());
        assert!(!none.view("a", Some("0.10.0"), Some(FwState::Valid)).available);
        let fw = Firmware::with_offer(Some(offer("0.11.0")), Timing::default());
        let v = fw.view("a", Some("0.10.0"), Some(FwState::Valid));
        assert!(v.available, "{v:?}");
        assert_eq!((v.installed.as_deref(), v.offered.as_deref()), (Some("0.10.0"), Some("0.11.0")));
        assert!(!fw.view("a", Some("0.11.0"), Some(FwState::Valid)).available);
        assert!(!fw.view("a", Some("0.12.0"), Some(FwState::Valid)).available);
        assert!(!fw.view("a", None, None).available);
        assert!(!fw.view("a", Some("0.10.0"), Some(FwState::PendingVerify)).available, "not while it is on trial");
        fw.set("a", Run::Restarting);
        assert!(fw.active("a") && !fw.view("a", Some("0.10.0"), Some(FwState::Valid)).available);
        fw.set("a", Run::Failed { why: "x".into() });
        assert!(!fw.active("a"), "a failure is not an attempt in progress");
        assert!(fw.view("a", Some("0.10.0"), Some(FwState::Valid)).available, "and may be retried");
    }

    #[test]
    fn upload_progress_is_a_percentage_and_waiting_has_none() {
        assert_eq!(Run::Uploading { sent: 50, total: 200 }.percent(), Some(25));
        assert_eq!(Run::Uploading { sent: 0, total: 0 }.percent(), None);
        assert_eq!(Run::Restarting.percent(), None);
        assert_eq!(Run::Trial.percent(), None);
    }
}
