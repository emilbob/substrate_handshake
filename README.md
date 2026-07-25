# substrate_handshake

A small Rust client that connects to a [Substrate](https://substrate.io) node over WebSocket, verifies which chain the node is actually serving, and queries its identity over JSON-RPC.

```
$ cargo run -- --node-address wss://rpc.polkadot.io \
               --genesis-hash 91b171bb158e2d3848fa23a9f1c25182fb8e20313b2c1eb49219da7a70ce90c3

INFO  Connecting to node at wss://rpc.polkadot.io
INFO  Genesis hash verified: 0x91b171bb158e2d3848fa23a9f1c25182fb8e20313b2c1eb49219da7a70ce90c3
INFO  Received response for request id 1: {"id":1,"jsonrpc":"2.0","result":"Parity Polkadot"}
INFO  Received response for request id 2: {"id":2,"jsonrpc":"2.0","result":"Polkadot"}
INFO  Received response for request id 3: {"id":3,"jsonrpc":"2.0","result":"1.24.0-660acefe665"}
INFO  Node information queried!
```

## What it does

1. **Connects** to the node's JSON-RPC endpoint over WebSocket (`ws://` or `wss://`).
2. **Checks chain identity** by asking for the hash of block 0 via `chain_getBlockHash` and comparing it to the `--genesis-hash` you supplied. A node on a different chain is rejected and the client exits non-zero without querying anything further.
3. **Queries node information** — `system_name`, `system_chain` and `system_version` — matching each response to its request by JSON-RPC id, since nodes are free to answer out of order (and do).
4. **Logs** every step through `env_logger`, so `RUST_LOG=debug` shows the full exchange.

### What it is not

This does **not** perform a Substrate peer-to-peer handshake. That is a libp2p protocol on the node's p2p port (30333 by default) and requires Noise encryption, multistream-select and yamux to reach — effectively a re-implementation of part of `sc-network`. What this client does instead is verify chain identity over the RPC endpoint, which is a genuine check but a different and weaker one: it confirms *which chain* you are talking to, not that the peer is an authenticated validator.

The `HandshakeMessage` struct in `src/main.rs` is kept as a worked example of SCALE encoding — Substrate's wire codec — with a round-trip test. It is deliberately not sent to the node, because an RPC endpoint has no notion of it.

## Requirements

- Rust and Cargo ([rustup.rs](https://rustup.rs))
- A Substrate node to talk to — either a public endpoint or a local one (see below)

## Usage

```bash
cargo run -- [--node-address <url>] [--genesis-hash <hex>]
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--node-address` | `ws://127.0.0.1:9944` | WebSocket RPC endpoint. `wss://` is supported. |
| `--genesis-hash` | *(none)* | Hash the node must report, with or without a `0x` prefix. Omit to report the node's genesis hash without enforcing one. |

Exit code is `0` on success and `1` on any failure, including a genesis mismatch, so it is usable as a health check in scripts.

### Public endpoints

These genesis hashes were verified against the live networks:

| Chain | Endpoint | Genesis hash |
| --- | --- | --- |
| Polkadot | `wss://rpc.polkadot.io` | `91b171bb158e2d3848fa23a9f1c25182fb8e20313b2c1eb49219da7a70ce90c3` |
| Kusama | `wss://kusama-rpc.polkadot.io` | `b0a8d493285c2df73290dfb7e61f870f17b41801197a149ca93654499ea3dafe` |
| Westend | `wss://westend-rpc.polkadot.io` | `e143f23803ac50e8f6f8e62695d1ce9e4e1d68aa36c1cd2cfd15340213f3423e` |

Pointing the Polkadot hash at Kusama is rejected, which is the whole point of the check:

```
ERROR genesis hash mismatch — expected b0a8d493…3dafe, node reports 91b171bb…ce90c3
```

### A local node

The old `substrate-node-template` has been superseded by the [Polkadot SDK solochain template](https://github.com/paritytech/polkadot-sdk-solochain-template):

```bash
git clone https://github.com/paritytech/polkadot-sdk-solochain-template
cd polkadot-sdk-solochain-template
cargo build --release
./target/release/solochain-template-node --dev
```

Then, in another terminal:

```bash
cargo run          # defaults to ws://127.0.0.1:9944
```

A dev chain's genesis hash depends on its chainspec and runtime build, so there is no fixed value to check against — run without `--genesis-hash`, note the hash the client reports, and pass it back if you want the check enforced on later runs.

## Development

```bash
cargo test                      # unit tests, incl. mock-node integration
cargo clippy --all-targets      # lints
cargo fmt --check               # formatting
```

Tests run against an in-process mock WebSocket node, so they need no network and no running Substrate node. Timeout behaviour is tested with tokio's virtual time, so the suite finishes in milliseconds rather than waiting out real timeouts.

## Notes

- TLS uses `rustls` with the `ring` provider, so the build needs no OpenSSL system libraries.
- Both the connect and per-response paths are bounded by a 10-second timeout; a node that accepts the socket and goes silent produces an error rather than a hang.
