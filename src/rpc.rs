//! The transport: opening a WebSocket to the node and reading frames off it.
//!
//! Everything here is about *getting bytes to and from the node* — nothing in
//! this module knows what a genesis hash or a block header is. The JSON-RPC
//! calls built on top live in [`crate::probe`].

use futures_util::StreamExt;
use log::info;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

/// The client's end of the WebSocket connection to the node.
pub(crate) type NodeStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// JSON-RPC request ids, allocated here rather than per call site because they
/// share one connection: responses are matched by id, so a subscription must
/// not reuse the id of a query that may still be outstanding.
pub(crate) const ID_GENESIS: u64 = 0;
pub(crate) const ID_NAME: u64 = 1;
pub(crate) const ID_CHAIN: u64 = 2;
pub(crate) const ID_VERSION: u64 = 3;
pub(crate) const ID_HEALTH: u64 = 4;
pub(crate) const ID_SUBSCRIBE: u64 = 5;
pub(crate) const ID_UNSUBSCRIBE: u64 = 6;

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
pub(crate) struct Timeouts {
    /// Waiting for the node to answer a request.
    pub(crate) rpc: Duration,
    /// Waiting for the chain to produce a block.
    pub(crate) head: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            rpc: RPC_TIMEOUT,
            head: SUBSCRIPTION_TIMEOUT,
        }
    }
}

/// Opens a WebSocket connection to the node.
///
/// Bounded by `CONNECT_TIMEOUT`, so an address that accepts TCP but never
/// completes the WebSocket handshake fails rather than hanging.
///
/// # Arguments
///
/// * `address` - The node's endpoint, `ws://` or `wss://`.
///
/// # Returns
///
/// The open connection, or an error naming the address that failed.
pub(crate) async fn connect(address: &str) -> Result<NodeStream, Box<dyn std::error::Error>> {
    info!("Connecting to node at {address}");

    let (ws_stream, response) = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(address))
        .await
        .map_err(|_| format!("timed out connecting to {address} after {CONNECT_TIMEOUT:?}"))?
        .map_err(|e| format!("failed to connect to {address}: {e}"))?;

    info!("Connected to the node with response: {response:?}");
    Ok(ws_stream)
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
pub(crate) async fn next_text_frame(
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
