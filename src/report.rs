//! The probe's findings and the requirements they are judged against.

use log::info;
use serde::Serialize;

use crate::error::{Failure, ProbeError};

/// Everything the probe learned about the node, and the exact shape `--json`
/// prints to stdout.
///
/// Absent fields are omitted rather than serialised as `null`, so a consumer can
/// tell "the node did not answer this" from "the node answered with nothing".
/// The report is filled in as the run progresses and is printed even when the
/// run fails, because a monitor wants to know how far the probe got.
#[derive(Debug, Default, Serialize)]
pub struct ProbeReport {
    /// The endpoint that was probed.
    pub endpoint: String,
    /// Whether every step succeeded. `false` means `error` is set.
    pub ok: bool,
    /// What kind of thing went wrong, for a consumer that needs to branch.
    /// `error` below says the same thing in prose and is free to be reworded;
    /// this is the part that is safe to alert on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The genesis hash the node reported, `0x`-prefixed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genesis_hash: Option<String>,
    /// True only when `--genesis-hash` was supplied *and* matched. Without the
    /// flag the hash above is reported but nothing is proven about it.
    pub genesis_verified: bool,
    /// Round-trip time of the `chain_getBlockHash` call — one honest latency
    /// sample, taken before the pipelined queries muddy the measurement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpc_latency_ms: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Connected peers, from `system_health`. Zero on a live network means the
    /// node is isolated, which is the failure this probe exists to catch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peers: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_syncing: Option<bool>,
    /// False for a dev chain running alone, which is why `peers: 0` is only
    /// alarming when this is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub should_have_peers: Option<bool>,
    /// The node's best block number. Compare it across runs, or against another
    /// node, to tell a stuck node from a healthy one — the header carries no
    /// timestamp, so the probe cannot judge staleness on its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_block: Option<u64>,
    /// How many headers `--follow` observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heads_followed: Option<u64>,
}

/// Decides whether the node meets the requirements the caller set.
///
/// This is what makes the exit code mean "the node is usable" rather than
/// merely "the node answered". Takes the filled-in report rather than the raw
/// query result, so that the findings are always recorded before they are
/// judged — a failed requirement must still leave a complete report behind.
///
/// A requirement that cannot be evaluated is a failure, not a pass: if the node
/// would not say how many peers it has, `--require-peers` has not been met, and
/// treating silence as success is how a probe comes to certify a broken node.
///
/// # Arguments
///
/// * `report` - What the probe found.
/// * `min_peers` - The minimum connected peers required, if any.
/// * `require_synced` - Whether a syncing node should be rejected.
///
/// # Returns
///
/// A Result that is an error naming the first requirement the node failed.
pub fn check_requirements(
    report: &ProbeReport,
    min_peers: Option<u64>,
    require_synced: bool,
) -> Result<(), ProbeError> {
    let unmet = |message: String| ProbeError::new(Failure::RequirementUnmet, message);

    if let Some(min) = min_peers {
        match report.peers {
            Some(peers) if peers < min => {
                return Err(unmet(format!(
                    "node has {peers} peer(s), --require-peers demands {min}"
                )))
            }
            None => {
                return Err(unmet(
                    "node did not report a peer count, so --require-peers cannot be satisfied"
                        .into(),
                ))
            }
            Some(peers) => info!("Peer requirement met: {peers} >= {min}"),
        }
    }

    if require_synced {
        match report.is_syncing {
            Some(true) => return Err(unmet("node is still syncing".into())),
            None => {
                return Err(unmet(
                    "node did not report its sync state, so --require-synced cannot be satisfied"
                        .into(),
                ))
            }
            Some(false) => info!("Sync requirement met: node is not syncing"),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a report as if the node had answered with this health.
    fn report_with_health(peers: Option<u64>, is_syncing: Option<bool>) -> ProbeReport {
        ProbeReport {
            peers,
            is_syncing,
            ..Default::default()
        }
    }

    #[test]
    fn peer_requirement_accepts_enough_peers() {
        let report = report_with_health(Some(5), Some(false));
        assert!(
            check_requirements(&report, Some(5), false).is_ok(),
            "at bar"
        );
        assert!(check_requirements(&report, Some(4), false).is_ok(), "above");
        assert!(check_requirements(&report, None, false).is_ok(), "no bar");
    }

    #[test]
    fn peer_requirement_rejects_an_isolated_node() {
        let report = report_with_health(Some(0), Some(false));
        let err = check_requirements(&report, Some(1), false)
            .expect_err("a node with no peers must not pass");
        assert_eq!(
            err.kind(),
            Failure::RequirementUnmet,
            "a reachable node that fails a bar is not a connection problem"
        );
        assert!(
            err.to_string().contains("0 peer(s)"),
            "unhelpful error: {err}"
        );
    }

    /// The trap this whole flag exists to avoid: a node that will not say how
    /// many peers it has has not proven it has any.
    #[test]
    fn unreported_peer_count_fails_the_requirement() {
        let report = report_with_health(None, Some(false));
        assert!(
            check_requirements(&report, Some(1), false).is_err(),
            "silence must not be read as success"
        );
    }

    #[test]
    fn sync_requirement_rejects_a_syncing_node() {
        let syncing = report_with_health(Some(50), Some(true));
        assert!(check_requirements(&syncing, None, true).is_err());
        assert!(
            check_requirements(&syncing, None, false).is_ok(),
            "syncing is only a failure when --require-synced was asked for"
        );

        let synced = report_with_health(Some(50), Some(false));
        assert!(check_requirements(&synced, None, true).is_ok());
    }

    #[test]
    fn unreported_sync_state_fails_the_requirement() {
        let report = report_with_health(Some(50), None);
        assert!(check_requirements(&report, None, true).is_err());
    }

    /// The JSON shape is this tool's machine-facing contract, so pin it: absent
    /// findings are omitted rather than null, and `ok` is always present.
    #[test]
    fn json_report_omits_what_the_node_did_not_answer() {
        let report = ProbeReport {
            endpoint: "ws://127.0.0.1:9944".into(),
            ok: true,
            genesis_hash: Some("0xdead".into()),
            genesis_verified: true,
            peers: Some(3),
            ..Default::default()
        };

        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
        assert_eq!(json["ok"], true);
        assert_eq!(json["peers"], 3);
        assert_eq!(json["genesis_verified"], true);
        assert!(json.get("error").is_none(), "no error key on success");
        assert!(
            json.get("chain").is_none(),
            "an unanswered field must be omitted, not null"
        );
    }

    /// A failed run must still produce a report, or whatever is parsing stdout
    /// gets nothing exactly when the node is broken. The classification is the
    /// part a monitor branches on, so it has to be there and it has to be
    /// stable — never make a consumer regex the prose.
    #[test]
    fn json_report_carries_the_failure() {
        let report = ProbeReport {
            endpoint: "ws://127.0.0.1:9944".into(),
            ok: false,
            failure: Some(Failure::GenesisMismatch),
            error: Some("genesis hash mismatch — expected …".into()),
            ..Default::default()
        };

        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["failure"], "genesis_mismatch");
        assert_eq!(json["endpoint"], "ws://127.0.0.1:9944");
        assert!(json["error"].is_string());
    }

    /// The names are the machine-facing contract — renaming a variant silently
    /// breaks every alert built on it, so pin the wire strings.
    #[test]
    fn failure_kinds_serialise_to_stable_names() {
        let cases = [
            (Failure::Config, "config"),
            (Failure::Connect, "connect"),
            (Failure::Timeout, "timeout"),
            (Failure::Transport, "transport"),
            (Failure::Protocol, "protocol"),
            (Failure::RpcError, "rpc_error"),
            (Failure::GenesisMismatch, "genesis_mismatch"),
            (Failure::RequirementUnmet, "requirement_unmet"),
        ];
        for (kind, expected) in cases {
            assert_eq!(serde_json::to_value(kind).unwrap(), expected);
        }
    }

    /// A clean run must not carry a failure key at all.
    #[test]
    fn a_successful_report_has_no_failure() {
        let report = ProbeReport {
            ok: true,
            best_block: Some(32_263_282),
            ..Default::default()
        };

        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
        assert!(json.get("failure").is_none());
        assert!(json.get("error").is_none());
        assert_eq!(json["best_block"], 32_263_282u64);
    }
}
