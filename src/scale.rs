//! A worked example of SCALE, Substrate's wire codec.

use parity_scale_codec::{Decode, Encode};

/// A SCALE-encoded handshake message, kept as a worked example of Substrate's
/// wire codec (`parity-scale-codec`) — field order and types are load-bearing,
/// so encode and decode must agree exactly.
///
/// This is deliberately *not* sent to the node. A Substrate node's RPC endpoint
/// speaks JSON-RPC over text frames and has no notion of this struct; the real
/// peer handshake is a libp2p protocol on the p2p port (30333 by default),
/// which requires Noise, multistream-select and yamux to reach. Chain identity
/// is instead verified over RPC — see [`crate::probe::verify_genesis_hash`].
#[derive(Debug, PartialEq, Eq, Encode, Decode)]
pub struct HandshakeMessage {
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
    pub fn new(name: &str, chain: &str, genesis_hash: [u8; 32], capabilities: Vec<String>) -> Self {
        HandshakeMessage {
            version: 1,
            name: name.to_string(),
            chain: chain.to_string(),
            genesis_hash,
            capabilities,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::parse_genesis_hash;
    use crate::test_support::VALID_HASH;

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
}
