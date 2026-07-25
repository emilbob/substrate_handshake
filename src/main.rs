use clap::Parser;
use env_logger::Env;
use futures_util::{SinkExt, StreamExt};
use log::{error, info};
use parity_scale_codec::{Decode, Encode};
use serde_json::json;
use std::collections::HashSet;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

/// The client's end of the WebSocket connection to the node.
type NodeStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// How long to wait for the WebSocket connection to be established.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait for any single response frame from the node. Without this a
/// node that accepts the socket and then goes quiet would hang the client.
const RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait for a pushed block header. Deliberately far longer than
/// `RPC_TIMEOUT`: this waits on the chain's block time, not on the node being
/// responsive. Polkadot targets ~6s, but parachains and dev chains vary widely.
const SUBSCRIPTION_TIMEOUT: Duration = Duration::from_secs(120);

/// The network waits, grouped so they can be shortened in tests.
#[derive(Debug, Clone, Copy)]
struct Timeouts {
    /// Waiting for the node to answer a request.
    rpc: Duration,
    /// Waiting for the chain to produce a block.
    head: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            rpc: RPC_TIMEOUT,
            head: SUBSCRIPTION_TIMEOUT,
        }
    }
}

/// A SCALE-encoded handshake message, kept as a worked example of Substrate's
/// wire codec (`parity-scale-codec`) — field order and types are load-bearing,
/// so encode and decode must agree exactly.
///
/// This is deliberately *not* sent to the node. A Substrate node's RPC endpoint
/// speaks JSON-RPC over text frames and has no notion of this struct; the real
/// peer handshake is a libp2p protocol on the p2p port (30333 by default),
/// which requires Noise, multistream-select and yamux to reach. Chain identity
/// is instead verified over RPC — see [`check_genesis_hash`].
#[derive(Debug, PartialEq, Eq, Encode, Decode)]
struct HandshakeMessage {
    version: u32,
    name: String,
    chain: String,
    genesis_hash: [u8; 32],
    capabilities: Vec<String>,
}

impl HandshakeMessage {
    /// Creates a new HandshakeMessage.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the node.
    /// * `chain` - The chain the node is connected to.
    /// * `genesis_hash` - The genesis hash of the chain.
    /// * `capabilities` - The capabilities of the node.
    ///
    /// # Returns
    ///
    /// A HandshakeMessage instance.
    #[allow(dead_code)]
    fn new(name: &str, chain: &str, genesis_hash: [u8; 32], capabilities: Vec<String>) -> Self {
        HandshakeMessage {
            version: 1,
            name: name.to_string(),
            chain: chain.to_string(),
            genesis_hash,
            capabilities,
        }
    }
}

/// Reads the next JSON-RPC text frame from the node.
///
/// Non-text frames (ping/pong/binary) are skipped rather than treated as
/// responses. A closed connection or a silent node becomes an error instead of
/// an indefinite wait.
///
/// # Arguments
///
/// * `ws_stream` - The connection to read from.
/// * `timeout` - How long to wait. Answering a request and producing a block
///   are different waits, so the caller chooses.
///
/// # Returns
///
/// The body of the next text frame.
async fn next_text_frame(
    ws_stream: &mut NodeStream,
    timeout: Duration,
) -> Result<String, Box<dyn std::error::Error>> {
    loop {
        let msg = tokio::time::timeout(timeout, ws_stream.next())
            .await
            .map_err(|_| format!("node sent no response within {timeout:?}"))?
            .ok_or("connection closed by the node")??;

        match msg {
            Message::Text(text) => return Ok(text),
            Message::Close(_) => return Err("connection closed by the node".into()),
            _ => continue,
        }
    }
}

/// Checks which chain the node is serving.
///
/// Asks the node for the hash of block 0. When `expected` is given this is the
/// client's actual authenticity check: a node on a different chain — or an
/// endpoint that is not the intended one — reports a different genesis hash and
/// is rejected. When `expected` is `None` the hash is only reported, which is
/// what you want against a local dev chain whose genesis varies per chainspec.
///
/// # Arguments
///
/// * `ws_stream` - The connection to the node.
/// * `expected` - The genesis hash the caller requires, if any.
///
/// # Returns
///
/// A Result that is an error if the hashes differ or the node cannot answer.
async fn check_genesis_hash(
    ws_stream: &mut NodeStream,
    expected: Option<&[u8; 32]>,
    timeouts: Timeouts,
) -> Result<(), Box<dyn std::error::Error>> {
    const REQUEST_ID: u64 = 0;

    let request = json!({
        "jsonrpc": "2.0",
        "method": "chain_getBlockHash",
        "params": [0],
        "id": REQUEST_ID,
    });

    info!("Verifying genesis hash: {}", request);
    ws_stream.send(Message::Text(request.to_string())).await?;

    let response: serde_json::Value = loop {
        let text = next_text_frame(ws_stream, timeouts.rpc).await?;
        let value: serde_json::Value = serde_json::from_str(&text)?;
        if value["id"].as_u64() == Some(REQUEST_ID) {
            break value;
        }
        error!("Ignoring unexpected frame while verifying genesis hash: {value}");
    };

    if let Some(error) = response.get("error") {
        return Err(format!("node rejected chain_getBlockHash: {error}").into());
    }

    let reported = response["result"]
        .as_str()
        .ok_or("chain_getBlockHash returned no block hash — is block 0 available?")?;
    let reported = parse_genesis_hash(reported)
        .map_err(|e| format!("node reported an unusable genesis hash: {e}"))?;

    match expected {
        Some(expected) if reported != *expected => Err(format!(
            "genesis hash mismatch — expected {}, node reports {}",
            hex::encode(expected),
            hex::encode(reported)
        )
        .into()),
        Some(_) => {
            info!("Genesis hash verified: 0x{}", hex::encode(reported));
            Ok(())
        }
        None => {
            info!(
                "Node genesis hash is 0x{} (not verified — pass --genesis-hash to require one)",
                hex::encode(reported)
            );
            Ok(())
        }
    }
}

/// Queries node information from the Substrate node.
///
/// # Arguments
///
/// * `ws_stream` - The connection to the node.
///
/// # Returns
///
/// A Result indicating the success or failure of the query.
async fn query_node_info(
    ws_stream: &mut NodeStream,
    timeouts: Timeouts,
) -> Result<(), Box<dyn std::error::Error>> {
    let requests = vec![
        json!({
            "jsonrpc": "2.0",
            "method": "system_name",
            "params": [],
            "id": 1,
        }),
        json!({
            "jsonrpc": "2.0",
            "method": "system_chain",
            "params": [],
            "id": 2,
        }),
        json!({
            "jsonrpc": "2.0",
            "method": "system_version",
            "params": [],
            "id": 3,
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
                }
            }
            Some(id) => error!("Received response for unknown request id {}", id),
            None => error!("Received unexpected response: {}", response),
        }
    }

    Ok(())
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
/// A Result that is an error if the node refuses the subscription or stops
/// producing blocks.
async fn follow_new_heads(
    ws_stream: &mut NodeStream,
    count: u64,
    timeouts: Timeouts,
) -> Result<(), Box<dyn std::error::Error>> {
    const SUBSCRIBE_ID: u64 = 4;
    const UNSUBSCRIBE_ID: u64 = 5;

    let request = json!({
        "jsonrpc": "2.0",
        "method": "chain_subscribeNewHeads",
        "params": [],
        "id": SUBSCRIBE_ID,
    });
    info!("Subscribing to new heads: {}", request);
    ws_stream.send(Message::Text(request.to_string())).await?;

    // The subscription id arrives as the reply to the subscribe call; every
    // later notification carries it, which is how concurrent subscriptions on
    // one connection are told apart.
    let subscription_id = loop {
        let text = next_text_frame(ws_stream, timeouts.rpc).await?;
        let value: serde_json::Value = serde_json::from_str(&text)?;
        if value["id"].as_u64() != Some(SUBSCRIBE_ID) {
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
        "id": UNSUBSCRIBE_ID,
    });
    ws_stream.send(Message::Text(request.to_string())).await?;
    info!("Unsubscribed after {seen} header(s)");

    Ok(())
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
fn parse_genesis_hash(hex_str: &str) -> Result<[u8; 32], Box<dyn std::error::Error>> {
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
}

/// Connects to the node, verifies its chain identity and queries node info.
///
/// # Arguments
///
/// * `opt` - The parsed command-line arguments.
///
/// # Returns
///
/// A Result indicating the success or failure of the run.
async fn run(opt: Opt) -> Result<(), Box<dyn std::error::Error>> {
    let timeouts = Timeouts::default();
    let expected_genesis = opt
        .genesis_hash
        .as_deref()
        .map(parse_genesis_hash)
        .transpose()?;

    info!("Connecting to node at {}", opt.node_address);
    let (mut ws_stream, response) =
        tokio::time::timeout(CONNECT_TIMEOUT, connect_async(&opt.node_address))
            .await
            .map_err(|_| {
                format!(
                    "timed out connecting to {} after {CONNECT_TIMEOUT:?}",
                    opt.node_address
                )
            })?
            .map_err(|e| format!("failed to connect to {}: {e}", opt.node_address))?;
    info!("Connected to the node with response: {:?}", response);

    check_genesis_hash(&mut ws_stream, expected_genesis.as_ref(), timeouts).await?;
    query_node_info(&mut ws_stream, timeouts).await?;
    info!("Node information queried!");

    if let Some(count) = opt.follow {
        follow_new_heads(&mut ws_stream, count, timeouts).await?;
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

    if let Err(e) = run(Opt::parse()).await {
        error!("{e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    const VALID_HASH: &str = "5972ecbfbc42507482dbcb0a2892bcd70161fd9acdfdf7e6455ab39bac3dfb83";
    const OTHER_HASH: &str = "91b171bb158e2d3848fa23a9f1c25182fb8e20313b2c1eb49219da7a70ce90c3";

    /// Starts a mock node driven by `serve` and returns a connection to it.
    async fn mock_node<S, Fut>(serve: S) -> NodeStream
    where
        S: FnOnce(WebSocketStream<TcpStream>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            serve(accept_async(tcp).await.unwrap()).await;
        });

        connect_async(format!("ws://{addr}")).await.unwrap().0
    }

    /// Short real timeouts. Virtual time (`start_paused`) is unusable here:
    /// tokio auto-advances whenever the runtime is idle, and a socket read that
    /// has not arrived yet counts as idle — so the clock jumps past the timeout
    /// while the mock's reply is still in flight, and the test fails at random.
    fn fast() -> Timeouts {
        Timeouts {
            rpc: Duration::from_millis(250),
            head: Duration::from_millis(250),
        }
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

    /// The SCALE struct is documentation now, so pin its round-trip behaviour.
    #[test]
    fn handshake_message_scale_round_trip() {
        let original = HandshakeMessage::new(
            "my-node",
            "my-chain",
            parse_genesis_hash(VALID_HASH).unwrap(),
            vec!["full".to_string()],
        );
        let encoded = original.encode();
        let decoded = HandshakeMessage::decode(&mut &encoded[..]).unwrap();
        assert_eq!(original, decoded);
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
        ws.send(Message::Text(
            r#"{"jsonrpc":"2.0","id":4,"result":"sub-abc"}"#.into(),
        ))
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
            ws.send(Message::Text(
                r#"{"jsonrpc":"2.0","id":4,"result":"sub-abc"}"#.into(),
            ))
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
            ws.send(Message::Text(
                r#"{"jsonrpc":"2.0","id":4,"error":{"code":-32601,"message":"unsafe"}}"#.into(),
            ))
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

    /// The node hangs up after answering only one of the three requests. The
    /// terminated stream must surface an error, not spin on `None` forever.
    #[tokio::test]
    async fn hangup_errors_instead_of_spinning() {
        let mut ws = mock_node(|mut ws| async move {
            for _ in 0..3 {
                ws.next().await;
            }
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
            for id in 1..=3 {
                ws.next().await;
                ws.send(Message::Text(format!(
                    r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32601}}}}"#
                )))
                .await
                .unwrap();
            }
        })
        .await;

        assert!(
            query_node_info(&mut ws, fast()).await.is_ok(),
            "errored responses are still responses"
        );
    }
}
