use env_logger::Env;
use futures_util::{SinkExt, StreamExt};
use log::{error, info};
use parity_scale_codec::{Decode, Encode};
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use structopt::StructOpt;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

/// A struct representing the handshake message.
#[derive(Debug, Encode, Decode)]
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

/// Performs a handshake with the Substrate node.
///
/// # Arguments
///
/// * `ws_stream` - A WebSocket stream wrapped in a Mutex and Arc for thread safety.
/// * `genesis_hash` - The genesis hash of the chain.
///
/// # Returns
///
/// A Result indicating the success or failure of the handshake.
async fn perform_handshake(
    ws_stream: Arc<Mutex<WebSocketStream<MaybeTlsStream<TcpStream>>>>,
    genesis_hash: &[u8; 32],
) -> Result<(), Box<dyn std::error::Error>> {
    let capabilities = vec!["full".to_string()];
    let handshake_msg = HandshakeMessage::new("my-node", "my-chain", *genesis_hash, capabilities);
    let encoded_msg = handshake_msg.encode();

    let mut ws_stream = ws_stream.lock().await;
    ws_stream.send(Message::Binary(encoded_msg)).await?;

    if let Some(msg) = ws_stream.next().await {
        let msg = msg?;
        if let Message::Binary(bin) = msg {
            let response: HandshakeMessage = HandshakeMessage::decode(&mut &bin[..])?;
            info!("Received handshake response: {:?}", response);
        }
    }

    Ok(())
}

/// Queries node information from the Substrate node.
///
/// # Arguments
///
/// * `ws_stream` - A WebSocket stream wrapped in a Mutex and Arc for thread safety.
///
/// # Returns
///
/// A Result indicating the success or failure of the query.
async fn query_node_info(
    ws_stream: Arc<Mutex<WebSocketStream<MaybeTlsStream<TcpStream>>>>,
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

    let mut ws_stream = ws_stream.lock().await;

    for request in &requests {
        info!("Sending request: {}", request);
        ws_stream.send(Message::Text(request.to_string())).await?;
    }

    while !pending.is_empty() {
        let msg = match ws_stream.next().await {
            Some(msg) => msg?,
            // The node hung up before answering everything. Without this the loop
            // spins on a terminated stream forever.
            None => {
                return Err(format!(
                    "connection closed with {} request(s) unanswered: {:?}",
                    pending.len(),
                    pending
                )
                .into())
            }
        };

        if let Message::Text(response) = msg {
            let response: serde_json::Value = serde_json::from_str(&response)?;
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
    }

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

/// Struct to parse command-line arguments.
#[derive(StructOpt, Debug)]
#[structopt(name = "substrate_handshake")]
struct Opt {
    /// Node address to connect to
    #[structopt(long, default_value = "ws://127.0.0.1:9944")]
    node_address: String,

    /// Genesis hash of the chain
    #[structopt(
        long,
        default_value = "5972ecbfbc42507482dbcb0a2892bcd70161fd9acdfdf7e6455ab39bac3dfb83"
    )]
    genesis_hash: String,
}

/// The main function to run the program.
///
/// # Returns
///
/// A Result indicating the success or failure of the program.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    let opt = Opt::from_args();

    let genesis_hash = parse_genesis_hash(&opt.genesis_hash)?;

    info!("Connecting to node at {}", opt.node_address);
    let (ws_stream, _) = match connect_async(&opt.node_address).await {
        Ok((stream, response)) => {
            info!("Connected to the node with response: {:?}", response);
            (stream, response)
        }
        Err(e) => {
            error!("Failed to connect to the node: {}", e);
            return Err(Box::new(e) as Box<dyn std::error::Error>);
        }
    };

    let ws_stream = Arc::new(Mutex::new(ws_stream));

    if let Err(e) = perform_handshake(ws_stream.clone(), &genesis_hash).await {
        error!("Handshake failed: {}", e);
        return Err(e);
    }
    info!("Handshake completed!");

    if let Err(e) = query_node_info(ws_stream.clone()).await {
        error!("Querying node information failed: {}", e);
        return Err(e);
    }
    info!("Node information queried!");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;

    const VALID_HASH: &str = "5972ecbfbc42507482dbcb0a2892bcd70161fd9acdfdf7e6455ab39bac3dfb83";

    /// Runs `query_node_info` against a mock node driven by `serve`, failing the
    /// test rather than hanging the suite if the client never returns.
    async fn run_against_mock<F, Fut>(serve: F) -> Result<(), Box<dyn std::error::Error>>
    where
        F: FnOnce(WebSocketStream<tokio::net::TcpStream>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            serve(accept_async(tcp).await.unwrap()).await;
        });

        let (stream, _) = connect_async(format!("ws://{addr}")).await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            query_node_info(Arc::new(Mutex::new(stream))),
        )
        .await
        .expect("timed out — query_node_info never returned")
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

    /// The node hangs up after answering only one of the three requests. The
    /// terminated stream must surface an error, not spin on `None` forever.
    #[tokio::test]
    async fn hangup_errors_instead_of_spinning() {
        let result = run_against_mock(|mut ws| async move {
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

        assert!(result.is_err(), "early hangup should surface an error");
    }

    /// Every request is answered with a JSON-RPC error. Errors still retire the
    /// request they belong to, so the loop must terminate.
    #[tokio::test]
    async fn all_error_responses_still_terminate() {
        let result = run_against_mock(|mut ws| async move {
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

        assert!(result.is_ok(), "errored responses are still responses");
    }
}
