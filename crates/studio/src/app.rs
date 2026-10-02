//! Card 359: running as a Home Assistant app.
//!
//! The Supervisor starts the app's container with `SUPERVISOR_TOKEN` in its
//! environment and the app's options in `/data/options.json`. Seeing the token
//! is what **app mode** is; there is no wrapper script, so the binary stays the
//! single source of behaviour.
//!
//! # Ingress only, and why that is safe
//!
//! The app is `host_network: true` with `ingress: true`. For a host-network
//! app the Supervisor's ingress proxy connects to **the hassio bridge's
//! gateway address, `172.30.32.1`, on `ingress_port`**
//! (`supervisor/docker/app.py: ip_address` returns the network gateway when
//! `host_network` is set; `supervisor/api/ingress.py` builds
//! `http://{ip}:{ingress_port}/`). So in app mode the studio binds **that
//! address only** ([`INGRESS_ADDR`]), not `0.0.0.0`: an interface that exists
//! only inside the Home Assistant host, so the studio is not on the LAN at
//! all. On top of that the Home Assistant developer docs say "only connections
//! from `172.30.32.2` must be allowed", and so a peer check ([`only_peers`])
//! refuses every other peer with 403.
//!
//! Both are **app mode only**. A plain Docker deployment has no
//! `SUPERVISOR_TOKEN`, so [`plan`] returns `None`, `Config::peers` stays
//! `None`, no check is installed, and `SCREENY_LISTEN` means what it always
//! did.
//!
//! The option `direct_access` adds a second, unchecked listener on
//! `0.0.0.0:direct_port` (the port is its own, so it can never collide with
//! the ingress one).

use crate::ha::supervisor::Supervisor;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

/// The only peer ingress connections come from: the Supervisor.
pub const INGRESS_PEER: IpAddr = IpAddr::V4(Ipv4Addr::new(172, 30, 32, 2));
/// What the Supervisor connects to for a host-network app: the hassio
/// bridge's gateway.
pub const INGRESS_ADDR: Ipv4Addr = Ipv4Addr::new(172, 30, 32, 1);
/// `ingress_port` in `ha-app/config.yaml`. Not 8787, so that the direct
/// listener's port (8787 by default) never has to share it.
pub const INGRESS_PORT: u16 = 8099;
pub const DEFAULT_DIRECT_PORT: u16 = 8787;
/// Where the Supervisor writes the app's options.
pub const OPTIONS_PATH: &str = "/data/options.json";
pub const SUPERVISOR_URL: &str = "http://supervisor";

/// `options:` in `ha-app/config.yaml`, as `/data/options.json` has them.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Options {
    /// Also listen on the LAN, on `direct_port`, with no Home Assistant login
    /// in front. Off by default.
    pub direct_access: bool,
    pub direct_port: u16,
}

impl Default for Options {
    fn default() -> Self {
        Options { direct_access: false, direct_port: DEFAULT_DIRECT_PORT }
    }
}

/// Read `/data/options.json`'s text.
///
/// # Errors
///
/// Not JSON, or not the options.
pub fn parse_options(text: &str) -> Result<Options, String> {
    serde_json::from_str(text).map_err(|e| format!("options.json: {e}"))
}

/// What app mode decides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The ingress listener, peer-checked.
    pub listen: SocketAddr,
    pub peers: Vec<IpAddr>,
    /// The extra, unchecked, listener `direct_access` asks for.
    pub direct: Option<SocketAddr>,
    pub supervisor: Supervisor,
}

/// App mode, or `None` when this is not one (no `SUPERVISOR_TOKEN`).
///
/// `env` and the options text are passed in so tests can give their own.
/// `SCREENY_APP_INGRESS_LISTEN` and `SCREENY_APP_INGRESS_PEERS` exist only so
/// the ingress can be simulated on a machine that has no `172.30.32.1`;
/// `SCREENY_SUPERVISOR_URL` is how a test points at a fake Supervisor.
///
/// # Errors
///
/// A sentence for the log: unreadable options, or a `direct_port` that is the
/// ingress port.
pub fn plan(env: &dyn Fn(&str) -> Option<String>, options: Option<&str>) -> Result<Option<Plan>, String> {
    let Some(token) = env("SUPERVISOR_TOKEN") else { return Ok(None) };
    let options = match options {
        Some(text) => parse_options(text)?,
        None => Options::default(),
    };
    let listen = match env("SCREENY_APP_INGRESS_LISTEN") {
        Some(v) => v.parse().map_err(|_| format!("SCREENY_APP_INGRESS_LISTEN {v}: expected ADDR:PORT"))?,
        None => SocketAddr::from((INGRESS_ADDR, INGRESS_PORT)),
    };
    let peers = match env("SCREENY_APP_INGRESS_PEERS") {
        Some(v) => v
            .split(',')
            .map(|p| p.trim().parse::<IpAddr>().map_err(|_| format!("SCREENY_APP_INGRESS_PEERS {v}: expected addresses")))
            .collect::<Result<Vec<_>, _>>()?,
        None => vec![INGRESS_PEER],
    };
    let direct = if options.direct_access {
        if options.direct_port == 0 || options.direct_port == listen.port() {
            return Err(format!("direct_port {} cannot be used: it is 0 or the port ingress uses ({}).", options.direct_port, listen.port()));
        }
        Some(SocketAddr::from(([0, 0, 0, 0], options.direct_port)))
    } else {
        None
    };
    let base = env("SCREENY_SUPERVISOR_URL").unwrap_or_else(|| SUPERVISOR_URL.to_string());
    Ok(Some(Plan { listen, peers, direct, supervisor: Supervisor { base, token } }))
}

/// Refuse every peer that is not in the list: 403, with no body that says
/// more. Wrapped round the ingress listener's router only.
pub async fn only_peers(State(peers): State<Arc<Vec<IpAddr>>>, ConnectInfo(from): ConnectInfo<SocketAddr>, req: Request, next: Next) -> Response {
    // A v4 peer on a dual-stack socket arrives as `::ffff:a.b.c.d`.
    let ip = from.ip().to_canonical();
    if peers.contains(&ip) {
        next.run(req).await
    } else {
        (StatusCode::FORBIDDEN, "this studio is reached through Home Assistant").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| pairs.iter().find(|(key, _)| *key == k).map(|(_, v)| (*v).to_string())
    }

    #[test]
    fn no_token_is_not_app_mode_and_nothing_changes() {
        assert_eq!(plan(&env(&[]), None).unwrap(), None);
        assert_eq!(plan(&env(&[("SCREENY_LISTEN", "0.0.0.0:8787")]), Some(r#"{"direct_access":true}"#)).unwrap(), None);
    }

    #[test]
    fn options_json_is_parsed_with_defaults() {
        assert_eq!(parse_options("{}").unwrap(), Options::default());
        assert_eq!(parse_options(r#"{"direct_access":true,"direct_port":9000}"#).unwrap(), Options { direct_access: true, direct_port: 9000 });
        assert!(parse_options(r#"{"direct_port":"x"}"#).is_err());
        assert!(parse_options("nope").is_err());
        // Keys the Supervisor adds are not ours to refuse.
        assert_eq!(parse_options(r#"{"log_level":"info"}"#).unwrap(), Options::default());
    }

    #[test]
    fn by_default_only_the_supervisor_on_the_bridge_can_reach_it() {
        let p = plan(&env(&[("SUPERVISOR_TOKEN", "t")]), Some("{}")).unwrap().unwrap();
        assert_eq!(p.listen.to_string(), "172.30.32.1:8099");
        assert_eq!(p.peers, vec![INGRESS_PEER]);
        assert_eq!(p.direct, None);
        assert_eq!(p.supervisor.base, "http://supervisor");
    }

    #[test]
    fn direct_access_adds_a_lan_listener_on_its_own_port() {
        let p = plan(&env(&[("SUPERVISOR_TOKEN", "t")]), Some(r#"{"direct_access":true}"#)).unwrap().unwrap();
        assert_eq!(p.direct.unwrap().to_string(), "0.0.0.0:8787");
        assert_eq!(p.listen.port(), 8099, "ingress is untouched");
        let p = plan(&env(&[("SUPERVISOR_TOKEN", "t")]), Some(r#"{"direct_access":true,"direct_port":9000}"#)).unwrap().unwrap();
        assert_eq!(p.direct.unwrap().port(), 9000);
        // Off: the port is ignored.
        let p = plan(&env(&[("SUPERVISOR_TOKEN", "t")]), Some(r#"{"direct_port":9000}"#)).unwrap().unwrap();
        assert_eq!(p.direct, None);
        assert!(plan(&env(&[("SUPERVISOR_TOKEN", "t")]), Some(r#"{"direct_access":true,"direct_port":8099}"#)).is_err());
    }

    #[test]
    fn the_simulation_knobs_are_read() {
        let p = plan(
            &env(&[("SUPERVISOR_TOKEN", "t"), ("SCREENY_APP_INGRESS_LISTEN", "127.0.0.1:8801"), ("SCREENY_APP_INGRESS_PEERS", "127.0.0.1, 10.0.0.2"), ("SCREENY_SUPERVISOR_URL", "http://127.0.0.1:1")]),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!((p.listen.to_string().as_str(), p.peers.len(), p.supervisor.base.as_str()), ("127.0.0.1:8801", 2, "http://127.0.0.1:1"));
    }
}
