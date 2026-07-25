//! The JSON-RPC calls the probe makes, and what it reads out of the answers.
//!
//! Each function here is one step of the run: identify the chain, ask the node
//! about itself, watch it produce blocks. They take an already-open connection
//! from [`crate::rpc`] and know nothing about the CLI.

use futures_util::SinkExt;
use log::{error, info};
use serde_json::json;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::rpc::{
    next_text_frame, NodeStream, Timeouts, ID_CHAIN, ID_GENESIS, ID_HEALTH, ID_NAME, ID_SUBSCRIBE,
    ID_UNSUBSCRIBE, ID_VERSION,
};

/// What asking the node for block 0 produced.
#[derive(Debug)]
pub(crate) struct GenesisInfo {
    /// The hash the node reported.
    pub(crate) hash: [u8; 32],
    /// How long the node took to answer.
    pub(crate) latency: Duration,
}

/// What the node said about itself. Every field is optional: a node may reject
/// any single call — `system_health` in particular is not universally exposed —
/// and one refused query should not sink the whole probe.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct NodeInfo {
    pub(crate) name: Option<String>,
    pub(crate) chain: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) peers: Option<u64>,
    pub(crate) is_syncing: Option<bool>,
    pub(crate) should_have_peers: Option<bool>,
}

/// Parses a hex-encoded 32-byte genesis hash.
///
/// # Arguments
///
/// * `hex_str` - The genesis hash as a hex string, with or without a `0x` prefix.
///
/// # Returns
///
/// The decoded hash, or an error describing why the input was rejected.
pub(crate) fn parse_genesis_hash(hex_str: &str) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let hex_str = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    let bytes =
        hex::decode(hex_str).map_err(|e| format!("genesis hash is not valid hex: {}", e))?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
        format!(
            "genesis hash must be 32 bytes (64 hex chars), got {} bytes",
            bytes.len()
        )
        .into()
    })
}

/// Asks the node which chain it is serving, by requesting the hash of block 0.
///
/// Kept separate from [`verify_genesis_hash`] so that a mismatch can still be
/// reported *with* the hash the node gave: on the failure this check exists to
/// catch, "which chain is this really?" is the thing the caller most wants, and
/// it should not have to be parsed back out of an error message.
///
/// # Arguments
///
/// * `ws_stream` - The connection to the node.
///
/// # Returns
///
/// The hash the node reported and how long it took to answer.
pub(crate) async fn fetch_genesis_hash(
    ws_stream: &mut NodeStream,
    timeouts: Timeouts,
) -> Result<GenesisInfo, Box<dyn std::error::Error>> {
    let request = json!({
        "jsonrpc": "2.0",
        "method": "chain_getBlockHash",
        "params": [0],
        "id": ID_GENESIS,
    });

    info!("Verifying genesis hash: {}", request);
    // Timed here rather than around the whole function so the sample covers the
    // node's round trip and not our own JSON handling.
    let sent_at = Instant::now();
    ws_stream.send(Message::Text(request.to_string())).await?;

    let response: serde_json::Value = loop {
        let text = next_text_frame(ws_stream, timeouts.rpc).await?;
        let value: serde_json::Value = serde_json::from_str(&text)?;
        if value["id"].as_u64() == Some(ID_GENESIS) {
            break value;
        }
        error!("Ignoring unexpected frame while verifying genesis hash: {value}");
    };
    let latency = sent_at.elapsed();

    if let Some(error) = response.get("error") {
        return Err(format!("node rejected chain_getBlockHash: {error}").into());
    }

    let reported = response["result"]
        .as_str()
        .ok_or("chain_getBlockHash returned no block hash — is block 0 available?")?;
    let reported = parse_genesis_hash(reported)
        .map_err(|e| format!("node reported an unusable genesis hash: {e}"))?;

    Ok(GenesisInfo {
        hash: reported,
        latency,
    })
}

/// Decides whether the chain the node serves is the one the caller asked for.
///
/// When `expected` is given this is the client's actual authenticity check: a
/// node on a different chain — or an endpoint that is not the intended one —
/// reports a different genesis hash and is rejected. When `expected` is `None`
/// the hash is only reported, which is what you want against a local dev chain
/// whose genesis varies per chainspec.
///
/// # Arguments
///
/// * `reported` - The hash the node gave.
/// * `expected` - The genesis hash the caller requires, if any.
///
/// # Returns
///
/// Whether the hash was actually checked; `false` means no requirement was
/// supplied and nothing has been proven. An error if the two differ.
pub(crate) fn verify_genesis_hash(
    reported: [u8; 32],
    expected: Option<&[u8; 32]>,
) -> Result<bool, Box<dyn std::error::Error>> {
    match expected {
        Some(expected) if reported != *expected => Err(format!(
            "genesis hash mismatch — expected {}, node reports {}",
            hex::encode(expected),
            hex::encode(reported)
        )
        .into()),
        Some(_) => {
            info!("Genesis hash verified: 0x{}", hex::encode(reported));
            Ok(true)
        }
        None => {
            info!(
                "Node genesis hash is 0x{} (not verified — pass --genesis-hash to require one)",
                hex::encode(reported)
            );
            Ok(false)
        }
    }
}

/// Queries node identity and health from the Substrate node.
///
/// All four calls are sent before any reply is read, so the node answers them
/// in parallel and the whole step costs one round trip rather than four.
///
/// # Arguments
///
/// * `ws_stream` - The connection to the node.
///
/// # Returns
///
/// What the node reported. A call the node refuses leaves its field empty
/// rather than failing the probe; only a node that stops answering entirely is
/// an error.
pub(crate) async fn query_node_info(
    ws_stream: &mut NodeStream,
    timeouts: Timeouts,
) -> Result<NodeInfo, Box<dyn std::error::Error>> {
    let requests = vec![
        json!({
            "jsonrpc": "2.0",
            "method": "system_name",
            "params": [],
            "id": ID_NAME,
        }),
        json!({
            "jsonrpc": "2.0",
            "method": "system_chain",
            "params": [],
            "id": ID_CHAIN,
        }),
        json!({
            "jsonrpc": "2.0",
            "method": "system_version",
            "params": [],
            "id": ID_VERSION,
        }),
        // The one call here that reports a *state* rather than an identity: a
        // node can be reachable, correctly named, on the right chain, and still
        // useless because it has no peers or is still syncing.
        json!({
            "jsonrpc": "2.0",
            "method": "system_health",
            "params": [],
            "id": ID_HEALTH,
        }),
    ];

    // Track the ids we are still waiting on, so that a response — successful or
    // not — retires exactly the request it belongs to. Counting responses instead
    // would let an unsolicited message satisfy the loop.
    let mut pending: HashSet<u64> = requests
        .iter()
        .filter_map(|r| r["id"].as_u64())
        .collect::<HashSet<_>>();

    for request in &requests {
        info!("Sending request: {}", request);
        ws_stream.send(Message::Text(request.to_string())).await?;
    }

    let mut info = NodeInfo::default();

    while !pending.is_empty() {
        let text = next_text_frame(ws_stream, timeouts.rpc)
            .await
            .map_err(|e| {
                format!(
                    "{e} ({} request(s) unanswered: {:?})",
                    pending.len(),
                    pending
                )
            })?;

        let response: serde_json::Value = serde_json::from_str(&text)?;
        match response["id"].as_u64() {
            Some(id) if pending.remove(&id) => {
                if let Some(error) = response.get("error") {
                    error!("Error in response for request id {}: {}", id, error);
                } else {
                    info!("Received response for request id {}: {}", id, response);
                    record_response(&mut info, id, &response["result"]);
                }
            }
            Some(id) => error!("Received response for unknown request id {}", id),
            None => error!("Received unexpected response: {}", response),
        }
    }

    Ok(info)
}

/// Files one successful response into `info` under the request it answers.
///
/// A result of an unexpected shape is dropped with a log line rather than
/// failing the run: the node answered, it just answered oddly, and the rest of
/// the report is still worth having.
fn record_response(info: &mut NodeInfo, id: u64, result: &serde_json::Value) {
    let as_text = |value: &serde_json::Value| match value.as_str() {
        Some(text) => Some(text.to_string()),
        None => {
            error!("Response for request id {id} is not a string: {value}");
            None
        }
    };

    match id {
        ID_NAME => info.name = as_text(result),
        ID_CHAIN => info.chain = as_text(result),
        ID_VERSION => info.version = as_text(result),
        ID_HEALTH => {
            info.peers = result["peers"].as_u64();
            info.is_syncing = result["isSyncing"].as_bool();
            info.should_have_peers = result["shouldHavePeers"].as_bool();
            info!(
                "Node health: {} peer(s), syncing={:?}",
                info.peers
                    .map_or_else(|| "?".to_string(), |p| p.to_string()),
                info.is_syncing
            );
        }
        _ => error!("No handler for request id {id}"),
    }
}

/// Follows new block headers pushed by the node.
///
/// This is the one thing here that genuinely needs a WebSocket: the request and
/// response calls above would work as well over HTTP POST, but a subscription
/// has the node push frames unprompted for as long as the connection lives.
///
/// Subscribes with `chain_subscribeNewHeads`, reports `count` headers as they
/// arrive, then unsubscribes so the node stops sending.
///
/// # Arguments
///
/// * `ws_stream` - The connection to the node.
/// * `count` - How many headers to observe before unsubscribing.
///
/// # Returns
///
/// How many headers were observed, or an error if the node refuses the
/// subscription or stops producing blocks.
pub(crate) async fn follow_new_heads(
    ws_stream: &mut NodeStream,
    count: u64,
    timeouts: Timeouts,
) -> Result<u64, Box<dyn std::error::Error>> {
    let request = json!({
        "jsonrpc": "2.0",
        "method": "chain_subscribeNewHeads",
        "params": [],
        "id": ID_SUBSCRIBE,
    });
    info!("Subscribing to new heads: {}", request);
    ws_stream.send(Message::Text(request.to_string())).await?;

    // The subscription id arrives as the reply to the subscribe call; every
    // later notification carries it, which is how concurrent subscriptions on
    // one connection are told apart.
    let subscription_id = loop {
        let text = next_text_frame(ws_stream, timeouts.rpc).await?;
        let value: serde_json::Value = serde_json::from_str(&text)?;
        if value["id"].as_u64() != Some(ID_SUBSCRIBE) {
            continue;
        }
        if let Some(error) = value.get("error") {
            return Err(format!("node rejected chain_subscribeNewHeads: {error}").into());
        }
        break value["result"]
            .as_str()
            .ok_or("subscription returned no id")?
            .to_string();
    };
    info!("Subscribed with id {subscription_id}");

    let mut seen = 0;
    while seen < count {
        // A block is not a reply, so this waits on the chain's block time
        // rather than on the node's responsiveness.
        let text = next_text_frame(ws_stream, timeouts.head)
            .await
            .map_err(|e| format!("{e} (saw {seen} of {count} headers)"))?;
        let value: serde_json::Value = serde_json::from_str(&text)?;

        if value["method"].as_str() != Some("chain_newHead")
            || value["params"]["subscription"].as_str() != Some(&subscription_id)
        {
            continue;
        }

        let header = &value["params"]["result"];
        let number = header["number"]
            .as_str()
            .and_then(|n| u64::from_str_radix(n.trim_start_matches("0x"), 16).ok());
        match number {
            Some(number) => info!(
                "New head #{number} parent={}",
                header["parentHash"].as_str().unwrap_or("?")
            ),
            None => error!("New head with unreadable block number: {header}"),
        }
        seen += 1;
    }

    let request = json!({
        "jsonrpc": "2.0",
        "method": "chain_unsubscribeNewHeads",
        "params": [subscription_id],
        "id": ID_UNSUBSCRIBE,
    });
    ws_stream.send(Message::Text(request.to_string())).await?;
    info!("Unsubscribed after {seen} header(s)");

    Ok(seen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fast, mock_node, take_requests, OTHER_HASH, VALID_HASH};
    use futures_util::StreamExt;
    use tokio::net::TcpStream;
    use tokio_tungstenite::WebSocketStream;

    /// The genesis step as `run` composes it — fetch, then compare. Lets the
    /// tests below assert on the whole check rather than on either half.
    async fn check_genesis_hash(
        ws_stream: &mut NodeStream,
        expected: Option<&[u8; 32]>,
        timeouts: Timeouts,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let genesis = fetch_genesis_hash(ws_stream, timeouts).await?;
        verify_genesis_hash(genesis.hash, expected)
    }

    /// Answers a single `chain_getBlockHash` request with `hash`.
    async fn serve_genesis(mut ws: WebSocketStream<TcpStream>, hash: &str) {
        ws.next().await;
        ws.send(Message::Text(format!(
            r#"{{"jsonrpc":"2.0","id":0,"result":"0x{hash}"}}"#
        )))
        .await
        .unwrap();
        // Hold the connection open so the client's own logic decides the outcome.
        ws.next().await;
    }

    #[test]
    fn genesis_hash_rejects_bad_input_without_panicking() {
        assert!(parse_genesis_hash("zz").is_err(), "not hex");
        assert!(
            parse_genesis_hash("abcd").is_err(),
            "valid hex, wrong length"
        );
        assert_eq!(
            parse_genesis_hash(VALID_HASH).unwrap(),
            parse_genesis_hash(&format!("0x{VALID_HASH}")).unwrap(),
            "0x prefix should be accepted"
        );
    }

    #[tokio::test]
    async fn genesis_hash_matching_the_node_is_accepted() {
        let mut ws = mock_node(|ws| serve_genesis(ws, VALID_HASH)).await;
        let expected = parse_genesis_hash(VALID_HASH).unwrap();
        assert!(check_genesis_hash(&mut ws, Some(&expected), fast())
            .await
            .is_ok());
    }

    /// The check that gives `--genesis-hash` its meaning: a node on another
    /// chain must be rejected rather than queried.
    #[tokio::test]
    async fn genesis_hash_mismatch_is_rejected() {
        let mut ws = mock_node(|ws| serve_genesis(ws, OTHER_HASH)).await;
        let expected = parse_genesis_hash(VALID_HASH).unwrap();

        let err = check_genesis_hash(&mut ws, Some(&expected), fast())
            .await
            .expect_err("a different chain must not be accepted")
            .to_string();
        assert!(err.contains("mismatch"), "unhelpful error: {err}");
    }

    /// Without `--genesis-hash` there is nothing to enforce, so any chain is
    /// acceptable — this keeps a bare run against a local dev node working.
    #[tokio::test]
    async fn omitted_genesis_hash_accepts_any_chain() {
        let mut ws = mock_node(|ws| serve_genesis(ws, OTHER_HASH)).await;
        assert!(check_genesis_hash(&mut ws, None, fast()).await.is_ok());
    }

    #[tokio::test]
    async fn genesis_rpc_error_is_reported() {
        let mut ws = mock_node(|mut ws| async move {
            ws.next().await;
            ws.send(Message::Text(
                r#"{"jsonrpc":"2.0","id":0,"error":{"code":-32601}}"#.into(),
            ))
            .await
            .unwrap();
            ws.next().await;
        })
        .await;

        let expected = parse_genesis_hash(VALID_HASH).unwrap();
        assert!(check_genesis_hash(&mut ws, Some(&expected), fast())
            .await
            .is_err());
    }

    /// A node that accepts the socket and then says nothing must not hang the
    /// client.
    #[tokio::test]
    async fn silent_node_times_out() {
        let mut ws = mock_node(|mut ws| async move {
            // Accept the request, then never answer.
            ws.next().await;
            std::future::pending::<()>().await;
        })
        .await;

        let expected = parse_genesis_hash(VALID_HASH).unwrap();
        let err = check_genesis_hash(&mut ws, Some(&expected), fast())
            .await
            .expect_err("a silent node must time out")
            .to_string();
        assert!(err.contains("no response"), "unhelpful error: {err}");
    }

    /// Serves a new-heads subscription: confirms it, pushes `heads` headers
    /// starting at block 100, then waits for the unsubscribe.
    async fn serve_new_heads(mut ws: WebSocketStream<TcpStream>, heads: u64) {
        ws.next().await;
        ws.send(Message::Text(format!(
            r#"{{"jsonrpc":"2.0","id":{ID_SUBSCRIBE},"result":"sub-abc"}}"#
        )))
        .await
        .unwrap();

        for i in 0..heads {
            ws.send(Message::Text(format!(
                r#"{{"jsonrpc":"2.0","method":"chain_newHead","params":{{"subscription":"sub-abc","result":{{"number":"0x{:x}","parentHash":"0xdead"}}}}}}"#,
                100 + i
            )))
            .await
            .unwrap();
        }
        ws.next().await;
    }

    #[tokio::test]
    async fn follows_the_requested_number_of_heads() {
        let mut ws = mock_node(|ws| serve_new_heads(ws, 3)).await;
        assert!(follow_new_heads(&mut ws, 3, fast()).await.is_ok());
    }

    /// Notifications for a different subscription must not count — one
    /// connection can carry several.
    #[tokio::test]
    async fn ignores_other_subscriptions() {
        let mut ws = mock_node(|mut ws| async move {
            ws.next().await;
            ws.send(Message::Text(format!(
                r#"{{"jsonrpc":"2.0","id":{ID_SUBSCRIBE},"result":"sub-abc"}}"#
            )))
            .await
            .unwrap();
            // Another subscription's header, then ours.
            ws.send(Message::Text(
                r#"{"jsonrpc":"2.0","method":"chain_newHead","params":{"subscription":"sub-OTHER","result":{"number":"0x999","parentHash":"0x0"}}}"#.into(),
            ))
            .await
            .unwrap();
            ws.send(Message::Text(
                r#"{"jsonrpc":"2.0","method":"chain_newHead","params":{"subscription":"sub-abc","result":{"number":"0x64","parentHash":"0x0"}}}"#.into(),
            ))
            .await
            .unwrap();
            ws.next().await;
        })
        .await;

        assert!(
            follow_new_heads(&mut ws, 1, fast()).await.is_ok(),
            "the foreign notification should not have satisfied the count"
        );
    }

    #[tokio::test]
    async fn subscription_rejection_is_reported() {
        let mut ws = mock_node(|mut ws| async move {
            ws.next().await;
            ws.send(Message::Text(format!(
                r#"{{"jsonrpc":"2.0","id":{ID_SUBSCRIBE},"error":{{"code":-32601,"message":"unsafe"}}}}"#
            )))
            .await
            .unwrap();
            ws.next().await;
        })
        .await;

        let err = follow_new_heads(&mut ws, 1, fast())
            .await
            .expect_err("a refused subscription must not look like success")
            .to_string();
        assert!(err.contains("rejected"), "unhelpful error: {err}");
    }

    /// A chain that stalls mid-subscription must time out, and the error must
    /// say how far it got.
    #[tokio::test]
    async fn stalled_chain_times_out_and_reports_progress() {
        let mut ws = mock_node(|ws| serve_new_heads(ws, 1)).await;

        let err = follow_new_heads(&mut ws, 3, fast())
            .await
            .expect_err("a stalled chain must time out")
            .to_string();
        assert!(err.contains("saw 1 of 3"), "unhelpful error: {err}");
    }

    /// The node hangs up after answering only one of the four requests. The
    /// terminated stream must surface an error, not spin on `None` forever.
    #[tokio::test]
    async fn hangup_errors_instead_of_spinning() {
        let mut ws = mock_node(|mut ws| async move {
            take_requests(&mut ws, 4).await;
            ws.send(Message::Text(
                r#"{"jsonrpc":"2.0","id":1,"result":"node"}"#.into(),
            ))
            .await
            .unwrap();
            ws.close(None).await.unwrap();
        })
        .await;

        assert!(
            query_node_info(&mut ws, fast()).await.is_err(),
            "early hangup should surface an error"
        );
    }

    /// Every request is answered with a JSON-RPC error. Errors still retire the
    /// request they belong to, so the loop must terminate.
    #[tokio::test]
    async fn all_error_responses_still_terminate() {
        let mut ws = mock_node(|mut ws| async move {
            for id in 1..=4 {
                ws.next().await;
                ws.send(Message::Text(format!(
                    r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32601}}}}"#
                )))
                .await
                .unwrap();
            }
        })
        .await;

        let info = query_node_info(&mut ws, fast())
            .await
            .expect("errored responses are still responses");
        assert_eq!(info, NodeInfo::default(), "nothing was actually reported");
    }

    /// Answers all four identity/health calls, keyed by request id so the
    /// out-of-order case the client is built for is exercised (health first).
    async fn serve_node_info(mut ws: WebSocketStream<TcpStream>, health: &str) {
        let replies = [
            (ID_HEALTH, health.to_string()),
            (ID_VERSION, r#""1.24.0""#.to_string()),
            (ID_NAME, r#""Parity Polkadot""#.to_string()),
            (ID_CHAIN, r#""Polkadot""#.to_string()),
        ];
        take_requests(&mut ws, replies.len()).await;
        for (id, result) in replies {
            ws.send(Message::Text(format!(
                r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#
            )))
            .await
            .unwrap();
        }
        ws.next().await;
    }

    #[tokio::test]
    async fn query_reports_identity_and_health() {
        let mut ws = mock_node(|ws| {
            serve_node_info(
                ws,
                r#"{"peers":42,"isSyncing":false,"shouldHavePeers":true}"#,
            )
        })
        .await;

        let info = query_node_info(&mut ws, fast()).await.unwrap();
        assert_eq!(
            info,
            NodeInfo {
                name: Some("Parity Polkadot".into()),
                chain: Some("Polkadot".into()),
                version: Some("1.24.0".into()),
                peers: Some(42),
                is_syncing: Some(false),
                should_have_peers: Some(true),
            }
        );
    }

    /// `system_health` is not exposed everywhere. A node that refuses it is
    /// still worth reporting on, so the refusal must cost only that field.
    #[tokio::test]
    async fn refused_health_call_leaves_identity_intact() {
        let mut ws = mock_node(|mut ws| async move {
            take_requests(&mut ws, 4).await;
            for (id, body) in [
                (ID_NAME, r#""result":"Parity Polkadot""#),
                (ID_CHAIN, r#""result":"Polkadot""#),
                (ID_VERSION, r#""result":"1.24.0""#),
                (ID_HEALTH, r#""error":{"code":-32601}"#),
            ] {
                ws.send(Message::Text(format!(
                    r#"{{"jsonrpc":"2.0","id":{id},{body}}}"#
                )))
                .await
                .unwrap();
            }
            ws.next().await;
        })
        .await;

        let info = query_node_info(&mut ws, fast()).await.unwrap();
        assert_eq!(info.chain.as_deref(), Some("Polkadot"), "identity survived");
        assert_eq!(info.peers, None, "a refused call reports nothing");
        assert_eq!(info.is_syncing, None);
    }

    /// A health result missing fields must not be read as zero peers — "the
    /// node did not say" and "the node has no peers" mean opposite things.
    #[tokio::test]
    async fn partial_health_result_does_not_invent_values() {
        let mut ws = mock_node(|ws| serve_node_info(ws, r#"{"isSyncing":true}"#)).await;

        let info = query_node_info(&mut ws, fast()).await.unwrap();
        assert_eq!(info.is_syncing, Some(true));
        assert_eq!(info.peers, None, "absent must not become 0");
        assert_eq!(info.should_have_peers, None);
    }
}
