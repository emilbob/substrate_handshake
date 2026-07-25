//! Connects to a Substrate node, verifies which chain it serves, reports its
//! identity and health, and optionally follows new blocks.
//!
//! The probe itself lives here so that both front ends run *the same code*: the
//! `substrate-node-probe` CLI and the `serve` HTTP wrapper differ only in how
//! they collect a [`ProbeRequest`] and where they put the [`ProbeReport`].
//!
//! The steps are in [`probe`], the connection in [`rpc`], the findings in
//! [`report`], and the failure taxonomy in [`error`].

pub mod error;
pub mod guard;
pub mod probe;
pub mod report;
pub mod rpc;
pub mod scale;
#[cfg(test)]
mod test_support;

use log::info;

use error::ProbeError;
use probe::{
    fetch_genesis_hash, follow_new_heads, parse_genesis_hash, query_node_info, verify_genesis_hash,
};
use report::{check_requirements, ProbeReport};
use rpc::Timeouts;

/// What to probe, and what to demand of it.
///
/// Deliberately not the clap struct: the CLI is one caller of this and the HTTP
/// wrapper is another, so the request has to be expressible without a command
/// line.
#[derive(Debug, Clone, Default)]
pub struct ProbeRequest {
    /// The node's endpoint, `ws://` or `wss://`.
    pub endpoint: String,
    /// The genesis hash the node must report. `None` reports it without
    /// enforcing anything.
    pub genesis_hash: Option<String>,
    /// How many pushed headers to observe before unsubscribing.
    pub follow: Option<u64>,
    /// The minimum connected peers required.
    pub require_peers: Option<u64>,
    /// Whether a syncing node should be rejected.
    pub require_synced: bool,
}

/// Runs the probe, filling `report` as far as it gets.
///
/// Each step writes what it learned into `report` before the next one runs, so
/// a failure part-way through still leaves everything gathered up to that point
/// — which is most of the value when the probe is watching a node that broke.
///
/// # Arguments
///
/// * `req` - What to probe and what to require of it.
/// * `timeouts` - The network waits to apply.
/// * `report` - Filled in as the run progresses.
///
/// # Returns
///
/// A Result indicating the success or failure of the run.
pub async fn probe(
    req: &ProbeRequest,
    timeouts: Timeouts,
    report: &mut ProbeReport,
) -> Result<(), ProbeError> {
    let expected_genesis = req
        .genesis_hash
        .as_deref()
        .map(parse_genesis_hash)
        .transpose()
        .map_err(|e| ProbeError::config(format!("the required genesis hash is unusable: {e}")))?;

    let mut ws_stream = rpc::connect(&req.endpoint, timeouts.connect).await?;

    // Recorded before the comparison, so a mismatch is reported alongside the
    // hash that caused it rather than only in the error text.
    let genesis = fetch_genesis_hash(&mut ws_stream, timeouts).await?;
    report.genesis_hash = Some(format!("0x{}", hex::encode(genesis.hash)));
    report.rpc_latency_ms = Some(genesis.latency.as_millis());
    report.genesis_verified = verify_genesis_hash(genesis.hash, expected_genesis.as_ref())?;

    let info = query_node_info(&mut ws_stream, timeouts).await?;
    report.name = info.name;
    report.chain = info.chain;
    report.version = info.version;
    report.peers = info.peers;
    report.is_syncing = info.is_syncing;
    report.should_have_peers = info.should_have_peers;
    report.best_block = info.best_block;
    info!("Node information queried!");

    // Judged before `--follow`, which can block for as long as the chain takes
    // to produce a block: there is no sense watching heads on a node that has
    // already failed the bar the caller set.
    check_requirements(report, req.require_peers, req.require_synced)?;

    if let Some(count) = req.follow {
        report.heads_followed = Some(follow_new_heads(&mut ws_stream, count, timeouts).await?);
    }

    ws_stream.close(None).await.ok();
    Ok(())
}

/// Runs the probe and returns the report either way.
///
/// The report is the deliverable whether or not the run succeeded — a probe
/// that yields nothing precisely when the node is broken is useless to whatever
/// is reading it — so this folds the outcome in rather than making every caller
/// remember to.
pub async fn probe_to_report(req: &ProbeRequest, timeouts: Timeouts) -> ProbeReport {
    let mut report = ProbeReport {
        endpoint: req.endpoint.clone(),
        ..Default::default()
    };

    match probe(req, timeouts, &mut report).await {
        Ok(()) => report.ok = true,
        Err(e) => {
            report.failure = Some(e.kind());
            report.error = Some(e.to_string());
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLKADOT_GENESIS: &str =
        "91b171bb158e2d3848fa23a9f1c25182fb8e20313b2c1eb49219da7a70ce90c3";

    /// The whole probe against the real Polkadot network — the one check the
    /// mock node cannot make. The mock answers whatever the test file tells it
    /// to, so it can only prove the client is self-consistent; it would keep
    /// passing if a method were renamed, a result shape changed, or the `wss://`
    /// and rustls path broke. Ignored by default because it needs the network
    /// and depends on a third party being up:
    ///
    /// ```text
    /// cargo test -- --ignored --nocapture
    /// ```
    ///
    /// Run on a schedule by `.github/workflows/live-probe.yml`, never on a PR.
    #[tokio::test]
    #[ignore = "hits the live Polkadot network; run with `cargo test -- --ignored`"]
    async fn live_polkadot_endpoint_passes_every_check() {
        // Another test in the process may have installed it already; only a
        // genuine absence would matter, and that surfaces as a panic below.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let req = ProbeRequest {
            endpoint: "wss://rpc.polkadot.io".to_string(),
            genesis_hash: Some(POLKADOT_GENESIS.to_string()),
            follow: Some(1),
            require_peers: Some(1),
            require_synced: true,
        };

        let report = probe_to_report(&req, Timeouts::default()).await;

        assert!(report.ok, "live probe failed: {report:#?}");
        assert!(report.genesis_verified, "genesis should have been enforced");
        assert_eq!(report.chain.as_deref(), Some("Polkadot"));
        assert!(report.name.is_some(), "system_name went unanswered");
        assert!(report.version.is_some(), "system_version went unanswered");
        // The reason this test exists in its current form: system_health is the
        // newest call here and the one most likely to drift.
        assert!(
            report.peers.unwrap_or(0) > 0,
            "system_health reported no peers: {report:#?}"
        );
        assert_eq!(report.is_syncing, Some(false));
        assert!(
            report.best_block.unwrap_or(0) > 0,
            "chain_getHeader gave no height: {report:#?}"
        );
        assert_eq!(report.heads_followed, Some(1), "no header was pushed");
    }
}
