//! The transport: opening a WebSocket to the node and reading frames off it.
//!
//! Everything here is about *getting bytes to and from the node* — nothing in
//! this module knows what a genesis hash or a block header is. The JSON-RPC
//! calls built on top live in [`crate::probe`].

use futures_util::StreamExt;
use log::info;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::error::ProbeError;

/// The client's end of the WebSocket connection to the node.
pub type NodeStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// JSON-RPC request ids, allocated here rather than per call site because they
/// share one connection: responses are matched by id, so a subscription must
/// not reuse the id of a query that may still be outstanding.
pub const ID_GENESIS: u64 = 0;
pub const ID_NAME: u64 = 1;
pub const ID_CHAIN: u64 = 2;
pub const ID_VERSION: u64 = 3;
pub const ID_HEALTH: u64 = 4;
pub const ID_SUBSCRIBE: u64 = 5;
pub const ID_UNSUBSCRIBE: u64 = 6;
pub const ID_HEADER: u64 = 7;

/// Default wait for the WebSocket connection to be established.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default wait for a node to answer a request. Without a bound, a node that
/// accepts the socket and then goes quiet would hang the client.
pub const RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Default wait for a pushed block header. Deliberately far longer than
/// `RPC_TIMEOUT`: this waits on the chain's block time, not on the node being
/// responsive. Polkadot targets ~6s, but parachains and dev chains vary widely.
pub const SUBSCRIPTION_TIMEOUT: Duration = Duration::from_secs(120);

/// The network waits, grouped so they can be shortened in tests and overridden
/// from the command line.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// Waiting for the connection to be established.
    pub connect: Duration,
    /// Waiting for the node to answer a request.
    pub rpc: Duration,
    /// Waiting for the chain to produce a block.
    pub head: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            connect: CONNECT_TIMEOUT,
            rpc: RPC_TIMEOUT,
            head: SUBSCRIPTION_TIMEOUT,
        }
    }
}

/// A budget for a whole operation, rather than for one frame of it.
///
/// The distinction matters because the read loops skip frames they do not
/// recognise. Bounding each individual read would let a node that emits one
/// unrelated frame just inside the limit keep the probe waiting forever — every
/// single wait short, the total unbounded. A deadline is set once when the
/// operation starts and every read inside it draws down the same clock.
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    at: Instant,
    /// The budget it was created with, kept only so timeout messages can name
    /// the limit the caller actually set.
    budget: Duration,
}

impl Deadline {
    /// Starts a budget of `budget` from now.
    pub fn after(budget: Duration) -> Self {
        Deadline {
            at: Instant::now() + budget,
            budget,
        }
    }

    /// How much of the budget is left, or a timeout error if none is.
    pub fn remaining(&self) -> Result<Duration, ProbeError> {
        self.at
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| self.expired())
    }

    /// The error to report when this budget runs out.
    pub fn expired(&self) -> ProbeError {
        ProbeError::timeout(format!(
            "node sent no usable response within {:?}",
            self.budget
        ))
    }
}

/// Opens a WebSocket connection to the node.
///
/// # Arguments
///
/// * `address` - The node's endpoint, `ws://` or `wss://`.
/// * `timeout` - How long to allow for the connection to be established.
///
/// # Returns
///
/// The open connection, or an error naming the address that failed.
pub async fn connect(address: &str, timeout: Duration) -> Result<NodeStream, ProbeError> {
    info!("Connecting to node at {address}");

    let (ws_stream, response) = tokio::time::timeout(timeout, connect_async(address))
        .await
        .map_err(|_| {
            ProbeError::timeout(format!(
                "timed out connecting to {address} after {timeout:?}"
            ))
        })?
        .map_err(|e| ProbeError::connect(format!("failed to connect to {address}: {e}")))?;

    info!("Connected to the node with response: {response:?}");
    Ok(ws_stream)
}

/// Reads the next JSON-RPC text frame from the node.
///
/// Non-text frames (ping/pong/binary) are skipped rather than treated as
/// responses. A closed connection or a silent node becomes an error instead of
/// an indefinite wait, bounded by `deadline` — which the caller shares across
/// every read in one operation, so skipped frames cannot extend it.
///
/// # Arguments
///
/// * `ws_stream` - The connection to read from.
/// * `deadline` - The remaining budget for the operation this read belongs to.
///
/// # Returns
///
/// The body of the next text frame.
pub async fn next_text_frame(
    ws_stream: &mut NodeStream,
    deadline: Deadline,
) -> Result<String, ProbeError> {
    loop {
        let msg = tokio::time::timeout(deadline.remaining()?, ws_stream.next())
            .await
            .map_err(|_| deadline.expired())?
            .ok_or_else(|| ProbeError::transport("connection closed by the node"))??;

        match msg {
            Message::Text(text) => return Ok(text),
            Message::Close(_) => {
                return Err(ProbeError::transport("connection closed by the node"))
            }
            _ => continue,
        }
    }
}
