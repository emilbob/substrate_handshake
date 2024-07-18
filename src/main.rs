use futures_util::SinkExt;
use parity_scale_codec::{Decode, Encode};
use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

#[derive(Debug, Encode, Decode)]
struct HandshakeMessage {
    version: u32,
    name: String,
    chain: String,
    genesis_hash: [u8; 32],
    capabilities: Vec<String>,
}

impl HandshakeMessage {
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

async fn perform_handshake(
    ws_stream: Arc<Mutex<WebSocketStream<MaybeTlsStream<TcpStream>>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let genesis_hash_vec =
        hex::decode("5972ecbfbc42507482dbcb0a2892bcd70161fd9acdfdf7e6455ab39bac3dfb83")?;
    let genesis_hash: [u8; 32] = genesis_hash_vec
        .try_into()
        .expect("Invalid length for genesis hash");

    let capabilities = vec!["full".to_string()];

    let handshake_msg = HandshakeMessage::new("my-node", "my-chain", genesis_hash, capabilities);
    let encoded_msg = handshake_msg.encode();

    // Acquire the lock to get the WebSocketStream
    let mut ws_stream = ws_stream.lock().await;

    // Use the WebSocketStream directly to send the message
    ws_stream.send(Message::Binary(encoded_msg)).await?;

    if let Some(msg) = ws_stream.next().await {
        let msg = msg?;
        if let Message::Binary(bin) = msg {
            let response: HandshakeMessage = HandshakeMessage::decode(&mut &bin[..])?;
            println!("Received handshake response: {:?}", response);
        }
    }

    Ok(())
}

async fn query_node_info(
    ws_stream: Arc<Mutex<WebSocketStream<MaybeTlsStream<TcpStream>>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let request_id = 1;

    // Define the JSON-RPC requests
    let requests = vec![
        json!({
            "jsonrpc": "2.0",
            "method": "system_name",
            "params": [],
            "id": request_id,
        }),
        json!({
            "jsonrpc": "2.0",
            "method": "system_chain",
            "params": [],
            "id": request_id + 1,
        }),
        json!({
            "jsonrpc": "2.0",
            "method": "system_version",
            "params": [],
            "id": request_id + 2,
        }),
    ];

    // Acquire the lock to get the WebSocketStream
    let mut ws_stream = ws_stream.lock().await;

    // Send each request and wait for the response
    for request in requests {
        // Use the WebSocketStream directly to send the message
        ws_stream.send(Message::Text(request.to_string())).await?;

        if let Some(msg) = ws_stream.next().await {
            let msg = msg?;
            if let Message::Text(response) = msg {
                let response: serde_json::Value = serde_json::from_str(&response)?;
                if let Some(error) = response.get("error") {
                    println!("Error in response: {}", error);
                } else {
                    println!("Received response: {}", response);
                }
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "ws://127.0.0.1:9944"; // Local Substrate node address
    let (ws_stream, _) = connect_async(addr).await?;
    println!("Connected to the node!");

    let ws_stream = Arc::new(Mutex::new(ws_stream));

    perform_handshake(ws_stream.clone()).await?;
    println!("Handshake completed!");

    query_node_info(ws_stream.clone()).await?;
    println!("Node information queried!");

    Ok(())
}
