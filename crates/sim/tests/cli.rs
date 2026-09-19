//! The binary, run as a binary.
//!
//! Headless only: a test that opened a window would need a display, and the
//! card says tests must be headless. The window is exercised by hand (see
//! `crates/sim/README.md`), and everything it draws is unit-tested in
//! `src/window.rs` and `src/screens.rs`.

mod common;

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::*;
use screeny_proto::dec::codec;
use screeny_proto::F_KEY;

const BIN: &str = env!("CARGO_BIN_EXE_screeny-sim");

struct Running {
    child: Child,
    frame: SocketAddr,
    control: SocketAddr,
    out: BufReader<std::process::ChildStdout>,
}

impl Running {
    /// Start the binary on loopback with ephemeral ports and no mDNS, and
    /// wait for it to say where it is listening.
    fn start(extra: &[&str]) -> Running {
        let mut child = Command::new(BIN)
            .args([
                "--headless",
                "--no-mdns",
                "--bind",
                "127.0.0.1",
                "--frame-port",
                "0",
                "--control-port",
                "0",
            ])
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn screeny-sim");

        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        out.read_line(&mut line).expect("the banner line");
        // "screeny-sim: frames on 127.0.0.1:1234, control on 127.0.0.1:5678"
        let addrs: Vec<SocketAddr> = line
            .split_whitespace()
            .filter_map(|w| w.trim_end_matches(',').parse().ok())
            .collect();
        assert_eq!(addrs.len(), 2, "could not read the banner: {line:?}");
        Running {
            child,
            frame: addrs[0],
            control: addrs[1],
            out,
        }
    }

    fn wait(mut self) -> (bool, String) {
        let status = self.child.wait().expect("wait");
        let mut rest = String::new();
        let mut line = String::new();
        while self.out.read_line(&mut line).unwrap_or(0) > 0 {
            rest.push_str(&line);
            line.clear();
        }
        (status.success(), rest)
    }
}

#[test]
fn headless_logs_statistics_and_exits_when_told() {
    let mut run = Running::start(&["--exit-after", "1.5"]);
    let mut tx = Sender::new(run.frame);
    let ctrl = Ctrl::new(run.control);

    // Something for it to report on.
    for i in 0..30u8 {
        let (p, _) = solid([i * 8, 40, 90]);
        tx.send(codec::SOLID, F_KEY, &p);
        std::thread::sleep(Duration::from_millis(20));
    }
    // And a control op, so the "not acted on" path is exercised too.
    ctrl.call(&screeny_proto::control::Request::Ping, 1)
        .expect("PING answered by the real binary");

    run.child.wait().expect("it exits on its own");
    let mut log = String::new();
    let mut line = String::new();
    while run.out.read_line(&mut line).unwrap_or(0) > 0 {
        log.push_str(&line);
        line.clear();
    }

    assert!(log.contains("fps"), "expected a statistics line:\n{log}");
    assert!(log.contains("SOLID"), "expected the codec:\n{log}");
    assert!(log.contains("shown"), "expected the counters:\n{log}");
    assert!(
        log.contains("LIVE") || log.contains("HOLD"),
        "state:\n{log}"
    );
}

#[test]
fn dump_dir_writes_every_nth_displayed_frame() {
    let dir = std::env::temp_dir().join(format!(
        "screeny-sim-cli-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);

    let mut run = Running::start(&[
        "--exit-after",
        "1.5",
        "--dump-dir",
        dir.to_str().unwrap(),
        "--dump-every",
        "3",
    ]);
    let mut tx = Sender::new(run.frame);
    let mut expected = Vec::new();
    for i in 0..12u8 {
        let (p, e) = solid([i * 10, 7, 200]);
        tx.send(codec::SOLID, F_KEY, &p);
        if i % 3 == 0 {
            expected.push(e);
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    run.child.wait().expect("exit");

    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("the dump directory")
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    assert_eq!(files.len(), expected.len(), "one PNG in every three frames");

    for (path, want) in files.iter().zip(&expected) {
        let f = std::io::BufReader::new(std::fs::File::open(path).unwrap());
        let mut reader = png::Decoder::new(f).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (64, 32));
        assert_frames_eq(
            &buf[..info.buffer_size()],
            want,
            path.file_name().unwrap().to_str().unwrap(),
        );
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_real_devices_mdns_name_is_refused_before_anything_binds() {
    for name in ["screeny", "screeny-a4cf12", "SCREENY.local"] {
        let out = Command::new(BIN)
            .args(["--headless", "--instance", name, "--exit-after", "0.1"])
            .output()
            .expect("run");
        assert!(!out.status.success(), "{name} should have been refused");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("reserved"),
            "{name}: expected a clear refusal, got {err:?}"
        );
    }
}

#[test]
fn bad_options_are_refused_with_a_message() {
    let cases: &[(&[&str], &str)] = &[
        (&["--drop", "150"], "between 0 and 100"),
        (&["--levels", "1"], "at least 2"),
        (&["--idle", "sideways"], "unknown mode"),
        (&["--frame-port"], "needs a value"),
        (&["--nonsense"], "unknown option"),
        (&["--bind", "not-an-address"], "--bind"),
    ];
    for (args, want) in cases {
        let out = Command::new(BIN).args(*args).output().expect("run");
        assert!(!out.status.success(), "{args:?} should have failed");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(want), "{args:?}: got {err:?}");
    }

    let out = Command::new(BIN).arg("--help").output().expect("run");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("USAGE"));
}

#[test]
fn fault_flags_reach_the_running_device() {
    let run = Running::start(&["--exit-after", "1.2", "--drop", "60", "--verbose"]);
    let mut tx = Sender::new(run.frame);
    let (p, _) = solid([1, 2, 3]);
    for _ in 0..40 {
        tx.send(codec::SOLID, F_KEY, &p);
        std::thread::sleep(Duration::from_millis(10));
    }
    let (ok, log) = run.wait();
    assert!(ok);
    assert!(log.contains("gaps"), "expected the counters:\n{log}");
    // 60% loss of 40 frames leaves holes in the sequence, and the stats line
    // reports them.
    let gaps_line = log
        .lines()
        .find(|l| l.contains("gaps"))
        .expect("a stats line");
    assert!(
        !gaps_line.contains("gaps 0 "),
        "expected some gaps: {gaps_line}"
    );
}
