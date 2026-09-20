//! `screeny-studio`: run the studio and open a browser at it.

use screeny_studio::{Config, Studio};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
screeny-studio - design generative pieces for the panel, in a browser

    screeny-studio [--listen ADDR] [--ui-dir DIR]

    --listen ADDR   address to serve on (default 127.0.0.1:8787). Anything
                    other than a loopback address puts the studio - and the
                    panel it is streaming to - in reach of the whole network,
                    with no password: see crates/studio/README.md
    --ui-dir DIR    serve the UI from this directory instead of from the
                    binary, so an edit needs a reload rather than a rebuild
    -h, --help      this
";

fn main() -> ExitCode {
    let cfg = match parse(std::env::args().skip(1)) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("screeny-studio: {e}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("screeny-studio: starting the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    runtime.block_on(async move {
        let studio = match Studio::bind(cfg.clone()).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("screeny-studio: listening on {}: {e}", cfg.listen);
                return ExitCode::FAILURE;
            }
        };
        println!("studio: http://{}/", shown(studio.addr));
        if !studio.addr.ip().is_loopback() {
            println!("studio: reachable from the whole network, and there is no password yet.");
        }
        if let Some(dir) = &cfg.ui_dir {
            println!("studio: serving the UI from {} (reload to see an edit)", dir.display());
        }
        match studio.serve().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("screeny-studio: serving: {e}");
                ExitCode::FAILURE
            }
        }
    })
}

/// `0.0.0.0:8787` is not a thing to click on; say where to go instead.
fn shown(addr: SocketAddr) -> String {
    if addr.ip().is_unspecified() {
        format!("localhost:{}", addr.port())
    } else {
        addr.to_string()
    }
}

/// `Ok(None)` means `--help` was asked for.
fn parse(args: impl Iterator<Item = String>) -> Result<Option<Config>, String> {
    let mut cfg = Config::default();
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--listen" => {
                let v = value()?;
                cfg.listen = resolve(&v)?;
            }
            "--ui-dir" => {
                let v = value()?;
                let dir = PathBuf::from(&v);
                if !dir.is_dir() {
                    return Err(format!("--ui-dir {v}: not a directory"));
                }
                cfg.ui_dir = Some(dir);
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(Some(cfg))
}

/// `--listen` takes `HOST:PORT`, or a bare port for the usual local case.
fn resolve(v: &str) -> Result<SocketAddr, String> {
    if let Ok(addr) = v.parse::<SocketAddr>() {
        return Ok(addr);
    }
    if let Ok(port) = v.parse::<u16>() {
        return Ok(SocketAddr::from(([127, 0, 0, 1], port)));
    }
    Err(format!("--listen {v}: expected ADDR:PORT (for example 127.0.0.1:8787 or 0.0.0.0:8787)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Option<Config>, String> {
        parse(args.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn the_default_is_loopback_on_8787() {
        let cfg = parse_args(&[]).unwrap().unwrap();
        assert_eq!(cfg.listen.to_string(), "127.0.0.1:8787");
        assert!(cfg.ui_dir.is_none());
    }

    #[test]
    fn listen_takes_an_address_or_a_port() {
        assert_eq!(parse_args(&["--listen", "0.0.0.0:9000"]).unwrap().unwrap().listen.to_string(), "0.0.0.0:9000");
        assert_eq!(parse_args(&["--listen", "9000"]).unwrap().unwrap().listen.to_string(), "127.0.0.1:9000");
        assert!(parse_args(&["--listen", "nowhere"]).is_err());
        assert!(parse_args(&["--listen"]).is_err());
    }

    #[test]
    fn a_ui_dir_has_to_exist() {
        assert!(parse_args(&["--ui-dir", "ui"]).unwrap().unwrap().ui_dir.is_some());
        assert!(parse_args(&["--ui-dir", "no/such/place"]).is_err());
        assert!(parse_args(&["--frobnicate"]).is_err());
        assert!(parse_args(&["--help"]).unwrap().is_none());
    }
}
