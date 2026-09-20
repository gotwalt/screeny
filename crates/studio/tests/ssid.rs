//! **The network's name is in the status payload, and it must not leak.**
//!
//! `GET /api/v1/status` on the device carries the real SSID. `CLAUDE.md` is
//! plain about it: it may be on the owner's page, and it must never be written
//! into a tracked file, a fixture, a log line, a card or a commit message. Two
//! of those the compiler cannot help with - the studio's own log, and the
//! state file it writes - so they are checked here.
//!
//! It is run against the **real binary**, as a subprocess, rather than in
//! process: `eprintln!` goes to a file descriptor, and the only honest test of
//! "it never reaches the log" is to read that descriptor. A studio started
//! here has `--no-discover`, a state directory of its own, an ephemeral port
//! and a simulator on loopback to talk to; the guard kills it however the test
//! ends.
//!
//! The SSID used is a made-up one that appears nowhere else in the repo, so a
//! grep for it in the output cannot match something innocent.

mod common;

use common::{post, until_json, Temp};
use std::io::Read;
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use screeny_sim::{Config as SimConfig, SimDevice};

/// A network name nothing else in this repo says. Not a real one: the bench's
/// real SSID is not in git and is not in this worker's reach.
const SECRET: &str = "Zzyzx-Not-A-Real-Network-41";

/// The studio's default read is every ten seconds and its telemetry poll every
/// five, so the first facts arrive at about twelve seconds. Generous.
const PATIENCE: Duration = Duration::from_secs(90);

/// A child process that is killed whichever way the test ends - including a
/// panic, which is what stops a failing test leaving a studio running.
struct Guard {
    child: Child,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_ssid_never_reaches_the_log_or_the_state_file() {
    // A simulator whose network is called something unmistakable.
    let (_dev, frame, http) = start_sim();
    let state = Temp::new("ssid");

    // The product, as the owner runs it, minus the LAN: no discovery, its own
    // state directory, an ephemeral loopback port, and its status reads
    // pointed at the simulator rather than at port 80.
    let mut child = Guard {
        child: Command::new(env!("CARGO_BIN_EXE_screeny-studio"))
            .args([
                "--listen",
                "127.0.0.1:0",
                "--no-discover",
                "--state-dir",
                &state.0.to_string_lossy(),
                "--device-http-port",
                &http.to_string(),
            ])
            .env_remove("SCREENY_LISTEN")
            .env_remove("SCREENY_STATE_DIR")
            .env_remove("SCREENY_DEVICE_HTTP_PORT")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the studio binary starts"),
    };

    let at = listening_on(&mut child.child).await;
    let body = format!(r#"{{"to":"127.0.0.1:{frame}","name":"bench","play":false}}"#);
    assert_eq!(post(at, "/api/v1/devices/add", &body).await.status, 200);

    // **The page can see it.** That is the point of reading the status at all,
    // and it is what makes the two greps below worth anything.
    let seen = until_json(at, PATIENCE, "the device's own status", "/api/v1/status", |v| {
        !v["devices"][0]["facts"].is_null()
    })
    .await;
    assert_eq!(
        seen["devices"][0]["facts"]["ssid"], SECRET,
        "the owner's page is where the network name belongs"
    );

    // Stop it the way `docker stop` does, so the state file is written the way
    // it is in life rather than by a `kill -9`.
    stop_politely(&mut child.child);
    let (out, err) = drain(&mut child.child);

    // **Neither the log nor the state file.**
    assert!(
        !out.contains(SECRET) && !err.contains(SECRET),
        "the studio wrote the network's name to its own log:\n--- stdout ---\n{out}\n--- stderr ---\n{err}"
    );
    let file = state.0.join("state.json");
    let saved = std::fs::read_to_string(&file).unwrap_or_default();
    assert!(!saved.is_empty(), "the studio should have written {}", file.display());
    assert!(!saved.contains(SECRET), "the network's name is in the state file: {saved}");
    // ...nor anything else the device said about itself: these are live facts,
    // not state, and a state file is read back after a month.
    assert!(!saved.contains("stack_free"), "device facts do not belong in the state file: {saved}");
    assert!(!saved.contains("boot_id"), "device facts do not belong in the state file: {saved}");

    println!("ssid: {} bytes of log and {} bytes of state, neither naming the network", out.len() + err.len(), saved.len());
}

/// A simulator with a known frame port and a known HTTP port, mDNS off,
/// loopback only. `(device, frame port, http port)`.
fn start_sim() -> (SimDevice, u16, u16) {
    for i in 0..40u16 {
        let (frame, http) = (51_200 + i * 2, 51_400 + i);
        let cfg = SimConfig {
            frame_port: frame,
            control_port: frame + 1,
            http_port: http,
            http_port_explicit: true,
            wifi_ssid: SECRET.to_string(),
            ..SimConfig::for_test()
        };
        if let Ok(dev) = SimDevice::start_with(cfg, None) {
            return (dev, frame, http);
        }
    }
    panic!("no free simulator ports in 51200..51280 / 51400..51440");
}

/// Read the binary's own "studio: http://HOST:PORT/" line, which is where an
/// ephemeral port is announced. Bounded: a studio that never says it is
/// listening is a test failure, not a hang.
async fn listening_on(child: &mut Child) -> SocketAddr {
    use std::io::{BufRead, BufReader};
    let stdout = child.stdout.take().expect("piped stdout");
    let found = tokio::task::spawn_blocking(move || {
        let mut reader = BufReader::new(stdout);
        let mut seen = String::new();
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return (None, seen, reader.into_inner()),
                Ok(_) => {}
            }
            seen.push_str(&line);
            if let Some(rest) = line.trim().strip_prefix("studio: http://") {
                let addr = rest.trim_end_matches('/').parse::<SocketAddr>().ok();
                return (addr, seen, reader.into_inner());
            }
        }
    });
    let (addr, seen, stdout) = tokio::time::timeout(Duration::from_secs(60), found)
        .await
        .expect("the studio said where it is listening")
        .expect("the reader thread finished");
    child.stdout = Some(stdout);
    addr.unwrap_or_else(|| panic!("the studio never said where it was listening:\n{seen}"))
}

/// SIGTERM, then wait - the same signal `docker stop` sends, which is what
/// makes the studio write its state file before it goes.
fn stop_politely(child: &mut Child) {
    // `libc` is not a dependency of this crate and is not worth becoming one
    // for one signal; `kill(1)` is on every machine this runs on.
    let _ = Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

/// Everything the child said, now that it has stopped.
fn drain(child: &mut Child) -> (String, String) {
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut err);
    }
    (out, err)
}

/// The route the page reads is the *only* place the SSID appears, so the
/// device list route - which a script may log wholesale - carries it too and
/// nothing else does. This is the in-process half: cheap, and it pins the one
/// thing a `{:?}` would break.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn debug_output_redacts_the_ssid() {
    use screeny_device_api::reply::StatusReply;
    use screeny_studio::devices::{DeviceFacts, Registry};

    let golden: StatusReply =
        serde_json::from_str(include_str!("../../device-api/tests/golden/status.json")).expect("the golden status");
    let ssid = golden.ssid.clone().expect("the golden has an SSID");

    // The type the page is drawn from.
    let facts = DeviceFacts::of(golden.clone(), None, &mut None);
    let printed = format!("{facts:?}");
    assert!(!printed.contains(ssid.as_str()), "DeviceFacts::fmt printed the network's name: {printed}");
    assert!(printed.contains("<redacted>"), "...and it should say that it did not: {printed}");

    // And the record that holds it, which *is* derived and is printed by any
    // `{:?}` on the registry.
    let reg = Registry::new();
    let id = reg.add_manual("127.0.0.1:51999", "paper panel").expect("added");
    reg.heard_http(&id, golden).expect("recorded");
    let record = format!("{:?}", reg.get(&id).expect("there"));
    assert!(!record.contains(ssid.as_str()), "DeviceRecord's Debug printed the network's name: {record}");

    // But the page really can have it, from the route that draws the page.
    let facts = reg.get(&id).expect("there").facts.expect("facts");
    assert_eq!(
        serde_json::to_value(&facts).expect("json")["ssid"],
        serde_json::Value::String(ssid.to_string())
    );
}
