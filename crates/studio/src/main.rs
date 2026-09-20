//! `screeny-studio`: run the studio and open a browser at it.

use screeny_studio::{Config, Studio};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
screeny-studio - play generative pieces on the panels, and design them in a browser

    screeny-studio [--listen ADDR] [--state-dir DIR] [--ui-dir DIR] [--no-discover]

    --listen ADDR    address to serve on (default 127.0.0.1:8787, env
                     SCREENY_LISTEN). Anything other than a loopback address
                     puts the studio - and the panels it is streaming to - in
                     reach of the whole network, with no password: see
                     crates/studio/README.md
    --state-dir DIR  where to keep what plays where (default ./.screeny-studio,
                     env SCREENY_STATE_DIR). One small file, written
                     atomically; the studio resumes from it after a restart
    --ui-dir DIR     serve the UI from this directory instead of from the
                     binary, so an edit needs a reload rather than a rebuild
    --no-discover    do not browse for panels; use configured addresses only
    -h, --help       this

    SCREENY_STUDIO_FAULTS=1 also offers two pieces that misbehave on purpose
    (fault-panic, fault-stall), for watching the containment work.
";

fn main() -> ExitCode {
    let cfg = match parse(std::env::args().skip(1), &from_env) {
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
        if let Some(dir) = &cfg.state_dir {
            println!("studio: state in {}", dir.display());
        }
        if !cfg.discover {
            println!("studio: not browsing for panels; configured addresses only");
        }
        if cfg.fault_pieces {
            println!("studio: the fault pieces are offered (SCREENY_STUDIO_FAULTS=1)");
        }
        // Ctrl-C and SIGTERM stop cleanly, which releases the panel at once
        // instead of leaving it on the last frame until its stream timeout.
        studio.stop_on_signal();
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

/// The state directory a local run uses when nothing says otherwise. Card 107
/// mounts a volume at `/data` and sets `SCREENY_STATE_DIR` instead.
const DEFAULT_STATE_DIR: &str = ".screeny-studio";

/// `Ok(None)` means `--help` was asked for.
///
/// `env` is the environment to fall back on, passed in so the tests can give
/// their own rather than mutating the process's.
fn parse(args: impl Iterator<Item = String>, env: &dyn Fn(&str) -> Option<String>) -> Result<Option<Config>, String> {
    // Flags beat the environment, the environment beats the default.
    let mut cfg = Config { discover: true, ..Config::default() };
    if let Some(v) = env("SCREENY_LISTEN") {
        cfg.listen = resolve(&v).map_err(|e| e.replace("--listen", "SCREENY_LISTEN"))?;
    }
    cfg.state_dir = Some(PathBuf::from(env("SCREENY_STATE_DIR").unwrap_or_else(|| DEFAULT_STATE_DIR.to_string())));
    cfg.fault_pieces = env("SCREENY_STUDIO_FAULTS").as_deref() == Some("1");

    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--listen" => {
                let v = value()?;
                cfg.listen = resolve(&v)?;
            }
            "--state-dir" => {
                let v = value()?;
                cfg.state_dir = Some(PathBuf::from(v));
            }
            "--no-discover" => cfg.discover = false,
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

fn from_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
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
        parse(args.iter().map(|s| (*s).to_string()), &|_| None)
    }

    fn parse_env(args: &[&str], env: &[(&str, &str)]) -> Result<Option<Config>, String> {
        let env: Vec<(String, String)> = env.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect();
        parse(args.iter().map(|s| (*s).to_string()), &move |k| {
            env.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone())
        })
    }

    /// The order the orchestrator asked for: a flag beats the environment,
    /// and the environment beats the default.
    #[test]
    fn a_flag_beats_the_environment_which_beats_the_default() {
        let cfg = parse_env(&[], &[]).unwrap().unwrap();
        assert_eq!(cfg.listen.to_string(), "127.0.0.1:8787");
        assert_eq!(cfg.state_dir.as_deref().map(std::path::Path::to_string_lossy).as_deref(), Some(DEFAULT_STATE_DIR));
        assert!(cfg.discover, "the product browses; only tests do not");

        let cfg = parse_env(&[], &[("SCREENY_LISTEN", "0.0.0.0:9999"), ("SCREENY_STATE_DIR", "/data")]).unwrap().unwrap();
        assert_eq!(cfg.listen.to_string(), "0.0.0.0:9999");
        assert_eq!(cfg.state_dir.as_deref().map(std::path::Path::to_string_lossy).as_deref(), Some("/data"));

        let cfg = parse_env(
            &["--listen", "127.0.0.1:1234", "--state-dir", "/tmp/elsewhere", "--no-discover"],
            &[("SCREENY_LISTEN", "0.0.0.0:9999"), ("SCREENY_STATE_DIR", "/data")],
        )
        .unwrap()
        .unwrap();
        assert_eq!(cfg.listen.to_string(), "127.0.0.1:1234");
        assert_eq!(cfg.state_dir.as_deref().map(std::path::Path::to_string_lossy).as_deref(), Some("/tmp/elsewhere"));
        assert!(!cfg.discover);
    }

    /// A broken SCREENY_LISTEN says so in its own name, not in the flag's.
    #[test]
    fn a_bad_listen_in_the_environment_names_the_environment() {
        let e = parse_env(&[], &[("SCREENY_LISTEN", "nowhere")]).unwrap_err();
        assert!(e.contains("SCREENY_LISTEN"), "{e}");
    }

    #[test]
    fn the_fault_pieces_need_asking_for() {
        assert!(!parse_env(&[], &[]).unwrap().unwrap().fault_pieces);
        assert!(!parse_env(&[], &[("SCREENY_STUDIO_FAULTS", "0")]).unwrap().unwrap().fault_pieces);
        assert!(parse_env(&[], &[("SCREENY_STUDIO_FAULTS", "1")]).unwrap().unwrap().fault_pieces);
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
