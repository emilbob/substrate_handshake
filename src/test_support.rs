//! Shared fixtures for the mock-node tests.
//!
//! Compiled only under `cfg(test)`. The mock answers exactly what the calling
//! test tells it to, which is the limit of what these tests can prove — see the
//! live-network test in `main.rs` for the part they cannot cover.

use futures_util::StreamExt;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{accept_async, connect_async, WebSocketStream};

use crate::rpc::{NodeStream, Timeouts};

pub(crate) const VALID_HASH: &str =
    "5972ecbfbc42507482dbcb0a2892bcd70161fd9acdfdf7e6455ab39bac3dfb83";
pub(crate) const OTHER_HASH: &str =
    "91b171bb158e2d3848fa23a9f1c25182fb8e20313b2c1eb49219da7a70ce90c3";

/// Starts a mock node driven by `serve` and returns a connection to it.
pub(crate) async fn mock_node<S, Fut>(serve: S) -> NodeStream
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
pub(crate) fn fast() -> Timeouts {
    Timeouts {
        connect: Duration::from_millis(250),
        rpc: Duration::from_millis(250),
        head: Duration::from_millis(250),
    }
}

/// Drains `count` requests from the mock's side of the connection.
pub(crate) async fn take_requests(ws: &mut WebSocketStream<TcpStream>, count: usize) {
    for _ in 0..count {
        ws.next().await;
    }
}
