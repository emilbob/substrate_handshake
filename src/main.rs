//! The command line: what to probe, what to demand of it, and how to report.
//!
//! The probe itself lives in the library so that this and the `serve` HTTP
//! wrapper run the same code — see [`substrate_node_probe`].

use clap::Parser;
use env_logger::Env;
use log::error;
use std::time::Duration;

use substrate_node_probe::{
    probe_to_report,
    rpc::{Timeouts, CONNECT_TIMEOUT, RPC_TIMEOUT, SUBSCRIPTION_TIMEOUT},
    ProbeRequest,
};

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

    /// Seconds to allow for the connection to be established.
    #[arg(long, value_name = "SECS", default_value_t = CONNECT_TIMEOUT.as_secs(), value_parser = clap::value_parser!(u64).range(1..))]
    connect_timeout: u64,

    /// Seconds to allow the node to answer a request. Bounds each step as a
    /// whole, so a node dribbling unrelated frames cannot extend it.
    #[arg(long, value_name = "SECS", default_value_t = RPC_TIMEOUT.as_secs(), value_parser = clap::value_parser!(u64).range(1..))]
    rpc_timeout: u64,

    /// Seconds to allow for each pushed block header under `--follow`. Far
    /// longer than the RPC wait by default: this waits on the chain's block
    /// time, not on the node being responsive. Raise it for a slow parachain.
    #[arg(long, value_name = "SECS", default_value_t = SUBSCRIPTION_TIMEOUT.as_secs(), value_parser = clap::value_parser!(u64).range(1..))]
    head_timeout: u64,
}

impl Opt {
    /// The network waits this invocation asked for.
    fn timeouts(&self) -> Timeouts {
        Timeouts {
            connect: Duration::from_secs(self.connect_timeout),
            rpc: Duration::from_secs(self.rpc_timeout),
            head: Duration::from_secs(self.head_timeout),
        }
    }

    /// What to probe, stripped of anything specific to a command line.
    fn request(&self) -> ProbeRequest {
        ProbeRequest {
            endpoint: self.node_address.clone(),
            genesis_hash: self.genesis_hash.clone(),
            follow: self.follow,
            require_peers: self.require_peers,
            require_synced: self.require_synced,
        }
    }
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
    let report = probe_to_report(&opt.request(), opt.timeouts()).await;

    // Printed on failure too: a probe that emits nothing precisely when the node
    // is broken is useless to whatever is parsing it.
    if opt.json {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => error!("failed to serialise the probe report: {e}"),
        }
    }

    if let Some(e) = &report.error {
        error!("{e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CLI is a contract too. A zero timeout would make every run fail
    /// instantly, which is never what someone means by asking for one.
    #[test]
    fn timeout_flags_reject_zero() {
        assert!(
            Opt::try_parse_from(["substrate-node-probe", "--rpc-timeout", "0"]).is_err(),
            "a zero RPC timeout must not be accepted"
        );
        let opt = Opt::try_parse_from(["substrate-node-probe", "--rpc-timeout", "3"])
            .expect("a positive timeout is valid");
        assert_eq!(opt.timeouts().rpc, Duration::from_secs(3));
        assert_eq!(
            opt.timeouts().head,
            SUBSCRIPTION_TIMEOUT,
            "unset flags keep their defaults"
        );
    }

    /// The default endpoint is a local dev node, which the hosted service would
    /// refuse — the CLI deliberately has no such limit.
    #[test]
    fn defaults_target_a_local_dev_node() {
        let opt = Opt::try_parse_from(["substrate-node-probe"]).unwrap();
        assert_eq!(opt.request().endpoint, "ws://127.0.0.1:9944");
        assert!(opt.request().genesis_hash.is_none(), "nothing enforced");
    }
}
