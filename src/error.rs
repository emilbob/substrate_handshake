//! What went wrong, in a form a machine can branch on.
//!
//! The probe's whole output contract is that a consumer should never have to
//! parse prose to act — see the `failure` field on
//! [`ProbeReport`](crate::report::ProbeReport). A `Box<dyn Error>` carrying a
//! sentence cannot support that, so every fallible step returns a
//! [`ProbeError`] whose [`Failure`] says which *kind* of thing went wrong while
//! the message stays free to be as specific as it likes.

use serde::Serialize;
use std::fmt;

/// The class of failure, stable enough to alert on.
///
/// A monitor pages differently for "I cannot reach this node" than for "this
/// node is up but has no peers", and those two must be distinguishable without
/// matching on error text that is free to be reworded at any time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    /// The command line asked for something impossible; the node is blameless.
    Config,
    /// The connection was never established.
    Connect,
    /// A wait expired. The node may be up but is not answering in time.
    Timeout,
    /// The connection failed or closed part-way through.
    Transport,
    /// The node answered, but with something unusable — bad JSON, or a result
    /// of the wrong shape.
    Protocol,
    /// The node returned a JSON-RPC error for a call the probe needs.
    RpcError,
    /// The node is serving a different chain than the caller required.
    GenesisMismatch,
    /// The node is reachable and correct, but failed a `--require-*` bar.
    RequirementUnmet,
}

/// A failure, classified.
#[derive(Debug)]
pub struct ProbeError {
    kind: Failure,
    message: String,
}

impl ProbeError {
    pub fn new(kind: Failure, message: impl Into<String>) -> Self {
        ProbeError {
            kind,
            message: message.into(),
        }
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::new(Failure::Config, message)
    }

    pub fn connect(message: impl Into<String>) -> Self {
        Self::new(Failure::Connect, message)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(Failure::Timeout, message)
    }

    pub fn transport(message: impl Into<String>) -> Self {
        Self::new(Failure::Transport, message)
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(Failure::Protocol, message)
    }

    pub fn rpc(message: impl Into<String>) -> Self {
        Self::new(Failure::RpcError, message)
    }

    pub fn kind(&self) -> Failure {
        self.kind
    }

    /// Adds detail without changing the classification.
    ///
    /// Used where an inner step already knows *what* went wrong and the outer
    /// step knows *how far it had got* — a timeout is still a timeout when you
    /// note it happened on the second of three headers.
    pub fn context(mut self, extra: impl AsRef<str>) -> Self {
        self.message = format!("{} {}", self.message, extra.as_ref());
        self
    }
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProbeError {}

/// A malformed frame is the node's fault, not the transport's.
impl From<serde_json::Error> for ProbeError {
    fn from(e: serde_json::Error) -> Self {
        ProbeError::protocol(format!("node sent unparseable JSON: {e}"))
    }
}

/// A WebSocket error mid-run means the connection broke under us; failures to
/// *establish* one are classified at the call site in [`crate::rpc::connect`].
impl From<tokio_tungstenite::tungstenite::Error> for ProbeError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        ProbeError::transport(format!("websocket error: {e}"))
    }
}
