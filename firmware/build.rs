//! Supplies the WiFi credentials to the firmware at build time, from outside git.
//!
//! The credentials are compiled into the image (there is no runtime provisioning
//! yet; the plan is a captive portal), but they must never be committed. Lookup
//! order, first hit wins, and both values must come from the same place:
//!
//!   1. environment: `SCREENY_WIFI_SSID` and `SCREENY_WIFI_PASSWORD`
//!   2. `firmware/wifi.env`                  (gitignored; see `wifi.env.example`)
//!   3. `~/.config/screeny/wifi.env`         (outside the repo: shared by every
//!                                            git worktree, impossible to commit)
//!
//! File format: `KEY=value` lines, `#` comments, no quoting, no `export`.
//! `src/main.rs` reads the result with `env!`.

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
            "\n\nNo WiFi credentials for the firmware.\n\
             Copy firmware/wifi.env.example to firmware/wifi.env (gitignored) or to\n\
             ~/.config/screeny/wifi.env and fill in {SSID} and {PASSWORD},\n\
             or set both as environment variables.\n\n"
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

    println!("cargo:warning=WiFi credentials: SSID from {source}");
    println!("cargo:rustc-env={SSID}={ssid}");
    println!("cargo:rustc-env={PASSWORD}={password}");
}
