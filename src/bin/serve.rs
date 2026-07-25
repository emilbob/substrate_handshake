//! An HTTP wrapper so the browser page can run the *actual* probe.
//!
//! The page shipped in `web/` re-implements the checks in JavaScript, because a
//! browser cannot execute this binary. Served from here it does not have to:
//! the page and this API are the same origin, the page notices, and every check
//! runs through [`substrate_node_probe::probe`] — the same code the CLI runs.
//!
//! Everything here that is not routing is about the difference between a tool
//! you run and a tool strangers run: the endpoint is validated as public before
//! it is dialled, the work is capped, and the whole thing holds no secrets.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use axum::{
    extract::Json,
    http::{HeaderValue, Method, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use log::{info, warn};
use serde::Deserialize;
use tower_http::{
    cors::CorsLayer, limit::RequestBodyLimitLayer, services::ServeDir, timeout::TimeoutLayer,
};

use substrate_node_probe::{
    guard, probe_to_report,
    report::ProbeReport,
    rpc::{Timeouts, RPC_TIMEOUT, SUBSCRIPTION_TIMEOUT},
    ProbeRequest,
};

/// Ceilings on what one anonymous request may ask for.
///
/// The CLI has no such caps — you may wait two minutes a block on your own
/// machine if you like. Here every request occupies a connection on a small
/// instance, so the expensive knob is the one that gets clamped hardest.
const MAX_FOLLOW: u64 = 3;
const MAX_HEAD_WAIT: Duration = Duration::from_secs(30);
const MAX_REQUEST_BODY: usize = 4 * 1024;
/// Hard ceiling on a whole request, above whatever the probe's own waits allow,
/// so a wedged handler cannot hold a worker forever.
const REQUEST_DEADLINE: Duration = Duration::from_secs(75);

#[derive(Debug, Deserialize)]
struct ProbeBody {
    endpoint: String,
    #[serde(default)]
    genesis_hash: Option<String>,
    #[serde(default)]
    follow: Option<u64>,
    #[serde(default)]
    require_peers: Option<u64>,
    #[serde(default)]
    require_synced: bool,
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // rustls 0.23 requires a process-level crypto provider to be chosen before
    // the first TLS connection, or it panics rather than returning an error.
    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_err()
    {
        log::error!("failed to install the rustls crypto provider");
        std::process::exit(1);
    }

    // Render supplies PORT; default to something memorable for local runs.
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);

    // Same-origin is the normal case, since this serves the page too. The
    // allowance exists for the copy on GitHub Pages, which is a different
    // origin and would otherwise be unable to reach a backend at all.
    let cors = match std::env::var("ALLOWED_ORIGIN") {
        Ok(origin) => match origin.parse::<HeaderValue>() {
            Ok(value) => CorsLayer::new()
                .allow_origin(value)
                .allow_methods([Method::GET, Method::POST])
                .allow_headers([axum::http::header::CONTENT_TYPE]),
            Err(_) => {
                log::error!("ALLOWED_ORIGIN is not a valid header value");
                std::process::exit(1);
            }
        },
        Err(_) => CorsLayer::new()
            .allow_methods([Method::GET, Method::POST])
            .allow_headers([axum::http::header::CONTENT_TYPE]),
    };

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/probe", post(run_probe))
        // The page is served from here so that it and the API share an origin;
        // that is what lets the page detect a backend and switch engines.
        .fallback_service(ServeDir::new(web_root()))
        .layer(cors)
        .layer(RequestBodyLimitLayer::new(MAX_REQUEST_BODY))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::GATEWAY_TIMEOUT,
            REQUEST_DEADLINE,
        ));

    let addr = SocketAddr::from((IpAddr::from([0, 0, 0, 0]), port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            log::error!("could not bind {addr}: {e}");
            std::process::exit(1);
        }
    };

    info!("substrate-node-probe server listening on {addr}");
    if let Err(e) = axum::serve(listener, app).await {
        log::error!("server stopped: {e}");
        std::process::exit(1);
    }
}

/// Where the static page lives. Overridable so the binary runs from a source
/// checkout as well as from the container, where the path differs.
fn web_root() -> String {
    std::env::var("WEB_ROOT").unwrap_or_else(|_| "web".to_string())
}

/// Tells the page that a real backend is here, so it can offer to use it.
async fn health() -> impl IntoResponse {
    Json(serde_json::json!({
        "ok": true,
        "engine": "rust",
        "version": env!("CARGO_PKG_VERSION"),
        "max_follow": MAX_FOLLOW,
    }))
}

async fn run_probe(Json(body): Json<ProbeBody>) -> impl IntoResponse {
    // Checked before anything is dialled. A rejection here is a refusal to act
    // on the request at all, so it is a 400 rather than a probe report — the
    // node was never contacted and has nothing to say.
    if let Err(reason) = guard::public_websocket_endpoint(&body.endpoint).await {
        warn!("refused endpoint {}: {reason}", body.endpoint);
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false,
                "failure": "config",
                "error": reason,
                "endpoint": body.endpoint,
            })),
        );
    }

    let follow = body.follow.map(|n| n.min(MAX_FOLLOW)).filter(|n| *n > 0);
    let req = ProbeRequest {
        endpoint: body.endpoint,
        genesis_hash: body.genesis_hash.filter(|h| !h.trim().is_empty()),
        follow,
        require_peers: body.require_peers,
        require_synced: body.require_synced,
    };

    let timeouts = Timeouts {
        connect: RPC_TIMEOUT,
        rpc: RPC_TIMEOUT,
        // Clamped well below the CLI's default: a caller willing to wait two
        // minutes for one block can do that on their own machine.
        head: MAX_HEAD_WAIT.min(SUBSCRIPTION_TIMEOUT),
    };

    info!("probing {}", req.endpoint);
    let report: ProbeReport = probe_to_report(&req, timeouts).await;

    // 200 even when the probe failed: the request succeeded, and the report is
    // the answer. HTTP status describes the conversation with this service, not
    // the health of a third-party node.
    let value = serde_json::to_value(&report).unwrap_or_else(
        |e| serde_json::json!({ "ok": false, "failure": "protocol", "error": e.to_string() }),
    );
    (StatusCode::OK, Json(value))
}
