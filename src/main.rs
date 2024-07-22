use env_logger::Env;
use futures_util::SinkExt;
use log::{error, info};
use parity_scale_codec::{Decode, Encode};
use serde_json::json;
use std::sync::Arc;
use structopt::StructOpt;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
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

    let mut ws_stream = ws_stream.lock().await;

    for request in &requests {
        info!("Sending request: {}", request.to_string());
        ws_stream.send(Message::Text(request.to_string())).await?;
    }

    let mut received_responses = 0;

    while received_responses < requests.len() {
        if let Some(msg) = ws_stream.next().await {
            let msg = msg?;
            if let Message::Text(response) = msg {
                let response: serde_json::Value = serde_json::from_str(&response)?;
                if let Some(error) = response.get("error") {
                    error!(
                        "Error in response for request id {}: {}",
                        response["id"], error
                    );
                } else if let Some(id) = response.get("id") {
                    info!("Received response for request id {}: {}", id, response);
                    received_responses += 1;
                } else {
                    error!("Received unexpected response: {}", response);
                }
            }
        }
    }

    Ok(())
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

    let genesis_hash_vec = hex::decode(&opt.genesis_hash)?;
    let genesis_hash: [u8; 32] = genesis_hash_vec
        .try_into()
        .expect("Invalid length for genesis hash");

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
