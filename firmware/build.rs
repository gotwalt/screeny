//! Supplies the **bench override** WiFi credentials to the firmware, from outside git.
//!
//! Owner's decision, 2026-09-20: compiled-in credentials are gone from the shipping
//! firmware. The device joins from the settings store (`crates/settings` on the
//! `screeny` partition), and a compiled-in pair survives only behind the cargo
//! feature `bench-wifi`, which is **off by default**.
//!
//! With `bench-wifi` off this script looks at nothing: not the environment, not
//! `firmware/wifi.env`, not `~/.config/screeny/wifi.env`. It emits no
//! `rustc-env`, so `src/main.rs`'s `SSID` and `PASSWORD` constants - which are
//! `#[cfg(feature = "bench-wifi")]` - do not exist either, and any use of them
//! would fail to compile. That is the check: a default build cannot contain a
//! credential, because there is no name to reach one by.
//!
//! With `bench-wifi` on, lookup order, first hit wins, and both values must come
//! from the same place:
//!
//!   1. environment: `SCREENY_WIFI_SSID` and `SCREENY_WIFI_PASSWORD`
//!   2. `firmware/wifi.env`                  (gitignored; see `wifi.env.example`)
//!   3. `~/.config/screeny/wifi.env`         (outside the repo: shared by every
//!                                            git worktree, impossible to commit)
//!
//! File format: `KEY=value` lines, `#` comments, no quoting, no `export`.
//! `src/main.rs` reads the result with `env!`.
//!
//! Nothing here ever prints a credential. The one line it does print says which
//! *source* the SSID came from, never what it is.

use std::{env, fs, path::PathBuf};

const SSID: &str = "SCREENY_WIFI_SSID";
const PASSWORD: &str = "SCREENY_WIFI_PASSWORD";

fn parse(text: &str) -> (Option<String>, Option<String>) {
    let (mut ssid, mut password) = (None, None);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                SSID => ssid = Some(value.trim().to_string()),
                PASSWORD => password = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    (ssid, password)
}

fn main() {
    // Cargo sets this for every enabled feature. Checking it here, rather than
    // in the code below, is what makes "off means the files are never read"
    // true rather than merely intended.
    if env::var_os("CARGO_FEATURE_BENCH_WIFI").is_none() {
        println!(
            "cargo:warning=No WiFi credentials compiled in (feature `bench-wifi` is off). \
             The device joins from its settings store."
        );
        return;
    }

    println!("cargo:rerun-if-env-changed={SSID}");
    println!("cargo:rerun-if-env-changed={PASSWORD}");

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let mut files = vec![manifest.join("wifi.env")];
    if let Some(home) = env::var_os("HOME") {
        files.push(PathBuf::from(home).join(".config/screeny/wifi.env"));
    }
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
    }

    let mut found = match (env::var(SSID), env::var(PASSWORD)) {
        (Ok(s), Ok(p)) => Some((s, p, "the environment".to_string())),
        _ => None,
    };
    if found.is_none() {
        for f in &files {
            if let Ok(text) = fs::read_to_string(f) {
                if let (Some(s), Some(p)) = parse(&text) {
                    found = Some((s, p, f.display().to_string()));
                    break;
                }
            }
        }
    }

    let Some((ssid, password, source)) = found else {
        panic!(
            "\n\nFeature `bench-wifi` is on but there are no WiFi credentials.\n\
             Copy firmware/wifi.env.example to firmware/wifi.env (gitignored) or to\n\
             ~/.config/screeny/wifi.env and fill in {SSID} and {PASSWORD},\n\
             or set both as environment variables - or build without `bench-wifi`\n\
             and let the device join from its settings store.\n\n"
        );
    };
    // 802.11: SSID 1..=32 bytes; WPA2 passphrase 8..=63 characters.
    assert!(
        (1..=32).contains(&ssid.len()),
        "{SSID} from {source} must be 1-32 bytes"
    );
    assert!(
        (8..=63).contains(&password.len()),
        "{PASSWORD} from {source} must be 8-63 characters"
    );

    println!("cargo:warning=WiFi credentials: SSID from {source} (bench-wifi build)");
    println!("cargo:rustc-env={SSID}={ssid}");
    println!("cargo:rustc-env={PASSWORD}={password}");
}
