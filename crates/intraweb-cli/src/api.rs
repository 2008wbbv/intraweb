//! The local JSON API, and the static serving of your own corner of the web.
//!
//! This is the single source of truth for state. The browser dashboard and the
//! terminal UI are both thin clients over these endpoints, so the two can never
//! drift into disagreeing about who is on the network.
//!
//! Note what is *not* here: no endpoint proxies another node's content. The
//! roster hands out addresses, and the browser goes straight to the peer. A hub
//! that relayed content would become the thing this project exists to avoid.

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use intraweb_core::identity::PeerId;
use intraweb_core::peer::now_secs;
use intraweb_core::{Config, Identity, Store, Vault};
use intraweb_net::Roster;
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tower_http::services::ServeDir;

/// Dashboard assets are compiled in, so the binary really is the whole program.
const DASHBOARD_HTML: &str = include_str!("../web/index.html");
const DASHBOARD_CSS: &str = include_str!("../web/style.css");
const DASHBOARD_JS: &str = include_str!("../web/app.js");

#[derive(Clone)]
pub struct AppState {
    pub roster: Roster,
    pub identity: Arc<Identity>,
    pub store: Arc<Mutex<Store>>,
    pub config: Arc<Config>,
    pub vault: Arc<Vault>,
    pub started_at: u64,
}

#[derive(Serialize)]
struct Status {
    nickname: String,
    peer_id: String,
    fingerprint: String,
    is_hub: bool,
    hub_name: Option<String>,
    api_port: u16,
    transport_port: u16,
    peers_online: usize,
    hubs_online: usize,
    known_peers: u64,
    vault_path: String,
    uptime_secs: u64,
    now: u64,
}

async fn status(State(state): State<AppState>) -> impl IntoResponse {
    let known_peers = state
        .store
        .lock()
        .ok()
        .and_then(|store| store.known_peer_count().ok())
        .unwrap_or(0);

    axum::Json(Status {
        nickname: state.config.sanitized_nickname(),
        peer_id: state.identity.peer_id().to_hex(),
        fingerprint: state.identity.fingerprint(),
        is_hub: state.config.hub,
        hub_name: state.config.hub.then(|| state.config.hub_name.clone()),
        api_port: state.config.api_port,
        transport_port: state.config.transport_port,
        peers_online: state.roster.len(),
        hubs_online: state.roster.hubs().len(),
        known_peers,
        vault_path: state.vault.root().display().to_string(),
        uptime_secs: now_secs().saturating_sub(state.started_at),
        now: now_secs(),
    })
}

async fn peers(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(state.roster.peers())
}

/// Every hub in earshot. Plural on purpose: one vault, many neighborhoods.
async fn hubs(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(state.roster.hubs())
}

async fn hub_history(State(state): State<AppState>) -> impl IntoResponse {
    let history = state
        .store
        .lock()
        .ok()
        .and_then(|store| store.hub_history(20).ok())
        .unwrap_or_default();
    axum::Json(history)
}

/// Record that a human compared fingerprints out of band and they matched.
async fn verify_peer(
    State(state): State<AppState>,
    Path(peer_hex): Path<String>,
) -> impl IntoResponse {
    let Some(peer_id) = PeerId::parse_hex(&peer_hex) else {
        return (StatusCode::BAD_REQUEST, "not a valid peer id").into_response();
    };
    let Ok(store) = state.store.lock() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "keyring is unavailable").into_response();
    };
    match store.mark_verified(peer_id) {
        Ok(true) => (StatusCode::OK, "verified").into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "we have never seen that peer").into_response(),
        Err(err) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("could not verify: {err}")).into_response()
        }
    }
}

/// Run the network diagnosis on demand. Kept short so the dashboard can wait.
async fn doctor(State(state): State<AppState>) -> impl IntoResponse {
    match intraweb_net::doctor::run(state.config.beacon_port, Duration::from_secs(3)).await {
        Ok(report) => axum::Json(report).into_response(),
        Err(err) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("diagnosis failed: {err}")).into_response()
        }
    }
}

pub fn router(state: AppState) -> Router {
    let nickname = state.config.sanitized_nickname();

    // Your own site and files, served straight off the vault. This is the one
    // folder that follows you to every hub.
    let site = ServeDir::new(state.vault.site_dir());
    let files = ServeDir::new(state.vault.files_dir());

    Router::new()
        .route("/", get(|| async { Html(DASHBOARD_HTML) }))
        .route(
            "/style.css",
            get(|| async { ([("content-type", "text/css; charset=utf-8")], DASHBOARD_CSS) }),
        )
        .route(
            "/app.js",
            get(|| async {
                ([("content-type", "text/javascript; charset=utf-8")], DASHBOARD_JS)
            }),
        )
        .route("/api/status", get(status))
        .route("/api/peers", get(peers))
        .route("/api/hubs", get(hubs))
        .route("/api/hubs/history", get(hub_history))
        .route("/api/doctor", get(doctor))
        .route("/api/peers/{peer_id}/verify", post(verify_peer))
        .nest_service(&format!("/~{nickname}"), site)
        .nest_service("/files", files)
        .with_state(state)
}

/// Whether to try the clean, port-free URL.
///
/// A hub prefers port 80 so a newcomer can type a bare hostname. But a port the
/// operator actually chose always wins: silently ignoring `--port` sends someone
/// chasing a dead URL, which is far worse than a long one.
pub fn should_try_low_port(is_hub: bool, port_was_chosen: bool) -> bool {
    is_hub && !port_was_chosen
}

/// Bind the dashboard, preferring port 80 so URLs stay free of a port number.
///
/// Falling back rather than failing is deliberate: binding low ports needs
/// privilege, and an operator who ran the binary as themselves should still get
/// a working node, just at a longer URL.
pub async fn bind(preferred_low_port: bool, port: u16) -> Result<(tokio::net::TcpListener, u16)> {
    if preferred_low_port {
        if let Ok(listener) = tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], 80))).await
        {
            return Ok((listener, 80));
        }
        // Point at something that actually exists today rather than a
        // subcommand we have not built yet.
        tracing::warn!(
            "could not bind port 80, serving on {port} instead; to get the \
             shorter URL, grant the capability once with: \
             sudo setcap 'cap_net_bind_service=+ep' $(command -v intraweb)"
        );
    }
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port)))
        .await
        .with_context(|| format!("could not bind port {port}"))?;
    Ok((listener, port))
}

#[cfg(test)]
mod tests {
    use super::should_try_low_port;

    #[test]
    fn a_plain_hub_takes_the_clean_url() {
        assert!(should_try_low_port(true, false));
    }

    #[test]
    fn an_explicit_port_is_never_overridden() {
        // Regression: --port used to be swallowed by the port-80 preference,
        // leaving the operator curling a port nothing was listening on.
        assert!(!should_try_low_port(true, true));
    }

    #[test]
    fn ordinary_peers_never_reach_for_port_80() {
        assert!(!should_try_low_port(false, false));
        assert!(!should_try_low_port(false, true));
    }
}
