//! Connects to a Substrate node, verifies which chain it serves, reports its
//! identity and health, and optionally follows new blocks.
//!
//! This file is the command line and the order the steps run in. The steps
//! themselves live in [`probe`], the connection in [`rpc`], and the findings in
//! [`report`].

mod probe;
mod report;
mod rpc;
mod scale;
#[cfg(test)]
mod test_support;

use clap::Parser;
use env_logger::Env;
use log::{error, info};

use probe::{
    fetch_genesis_hash, follow_new_heads, parse_genesis_hash, query_node_info, verify_genesis_hash,
};
use report::{check_requirements, ProbeReport};
use rpc::Timeouts;

/// Connect to a Substrate node, verify which chain it serves, and query its
/// identity over JSON-RPC.
#[derive(Parser, Debug)]
#[command(name = "substrate-node-probe", version)]
struct Opt {
    /// Node address to connect to
    #[arg(long, default_value = "ws://127.0.0.1:9944")]
    node_address: String,

    /// Genesis hash the node must report, with or without a `0x` prefix. The
    /// client refuses to continue if the node reports a different one. Omit it
    /// to report the node's genesis hash without enforcing a value.
    #[arg(long)]
    genesis_hash: Option<String>,

    /// After querying, follow this many new block headers as the node pushes
    /// them, then unsubscribe and exit. Omit to exit straight after the query.
    #[arg(long, value_name = "COUNT")]
    follow: Option<u64>,

    /// Fail unless the node reports at least this many connected peers. A node
    /// can answer every query correctly and still be useless because it is
    /// talking to nobody.
    #[arg(long, value_name = "N")]
    require_peers: Option<u64>,

    /// Fail if the node is still syncing. A syncing node serves stale state,
    /// which is worse than an unreachable one because it looks fine.
    #[arg(long)]
    require_synced: bool,

    /// Print the findings to stdout as a single JSON object. Logs stay on
    /// stderr, so stdout carries nothing but the report and can be piped
    /// straight into `jq`.
    #[arg(long)]
    json: bool,
}

/// Connects to the node, verifies its chain identity and queries node info.
///
/// Each step writes what it learned into `report` before the next one runs, so
/// a failure part-way through still leaves everything gathered up to that point
/// — which is most of the value when the probe is watching a node that broke.
///
/// # Arguments
///
/// * `opt` - The parsed command-line arguments.
/// * `timeouts` - The network waits to apply.
/// * `report` - Filled in as the run progresses.
///
/// # Returns
///
/// A Result indicating the success or failure of the run.
async fn run(
    opt: &Opt,
    timeouts: Timeouts,
    report: &mut ProbeReport,
) -> Result<(), Box<dyn std::error::Error>> {
    let expected_genesis = opt
        .genesis_hash
        .as_deref()
        .map(parse_genesis_hash)
        .transpose()?;

    let mut ws_stream = rpc::connect(&opt.node_address).await?;

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
    info!("Node information queried!");

    // Judged before `--follow`, which can block for as long as the chain takes
    // to produce a block: there is no sense watching heads on a node that has
    // already failed the bar the caller set.
    check_requirements(report, opt.require_peers, opt.require_synced)?;

    if let Some(count) = opt.follow {
        report.heads_followed = Some(follow_new_heads(&mut ws_stream, count, timeouts).await?);
    }

    ws_stream.close(None).await.ok();
    Ok(())
}

/// The main function to run the program.
#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    // rustls 0.23 requires a process-level crypto provider to be chosen before
    // the first TLS connection, or it panics rather than returning an error.
    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_err()
    {
        error!("failed to install the rustls crypto provider");
        std::process::exit(1);
    }

    let opt = Opt::parse();
    let mut report = ProbeReport {
        endpoint: opt.node_address.clone(),
        ..Default::default()
    };

    let result = run(&opt, Timeouts::default(), &mut report).await;
    match &result {
        Ok(()) => report.ok = true,
        Err(e) => report.error = Some(e.to_string()),
    }

    // Printed on failure too: a probe that emits nothing precisely when the node
    // is broken is useless to whatever is parsing it.
    if opt.json {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => error!("failed to serialise the probe report: {e}"),
        }
    }

    if let Err(e) = result {
        error!("{e}");
        std::process::exit(1);
    }
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

        let opt = Opt {
            node_address: "wss://rpc.polkadot.io".to_string(),
            genesis_hash: Some(POLKADOT_GENESIS.to_string()),
            follow: Some(1),
            require_peers: Some(1),
            require_synced: true,
            json: false,
        };

        let mut report = ProbeReport {
            endpoint: opt.node_address.clone(),
            ..Default::default()
        };
        let result = run(&opt, Timeouts::default(), &mut report).await;

        assert!(
            result.is_ok(),
            "live probe failed: {:?}\nreport: {report:#?}",
            result.err()
        );
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
        assert_eq!(report.heads_followed, Some(1), "no header was pushed");
    }
}
