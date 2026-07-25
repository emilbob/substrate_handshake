# substrate-node-probe

A small Rust client that connects to a [Substrate](https://substrate.io) node over WebSocket, verifies which chain the node is actually serving, reports its identity and health over JSON-RPC, and can follow new blocks as they are produced.

> Previously `substrate_handshake`. The old URL redirects.

```
$ substrate-node-probe --node-address wss://rpc.polkadot.io \
               --genesis-hash 91b171bb158e2d3848fa23a9f1c25182fb8e20313b2c1eb49219da7a70ce90c3

INFO  Connecting to node at wss://rpc.polkadot.io
INFO  Genesis hash verified: 0x91b171bb158e2d3848fa23a9f1c25182fb8e20313b2c1eb49219da7a70ce90c3
INFO  Received response for request id 1: {"id":1,"jsonrpc":"2.0","result":"Parity Polkadot"}
INFO  Received response for request id 2: {"id":2,"jsonrpc":"2.0","result":"Polkadot"}
INFO  Received response for request id 3: {"id":3,"jsonrpc":"2.0","result":"1.24.0-660acefe665"}
INFO  Node health: 65 peer(s), syncing=Some(false)
INFO  Node information queried!
```

## What it does

1. **Connects** to the node's JSON-RPC endpoint over WebSocket (`ws://` or `wss://`).
2. **Checks chain identity** by asking for the hash of block 0 via `chain_getBlockHash` and comparing it to the `--genesis-hash` you supplied. A node on a different chain is rejected and the client exits non-zero without querying anything further.
3. **Queries node information** — `system_name`, `system_chain`, `system_version` and `system_health` — matching each response to its request by JSON-RPC id, since nodes are free to answer out of order (and do). All four go out before any reply is read, so the step costs one round trip rather than four.
4. **Follows new blocks**, optionally — with `--follow N` it subscribes via `chain_subscribeNewHeads`, reports headers as the node pushes them, then unsubscribes.
5. **Reports**, either as logs through `env_logger` (`RUST_LOG=debug` shows the full exchange) or as a single JSON object with `--json`.

### Machine-readable output

`--json` prints one object to stdout. Logging stays on stderr, so stdout carries nothing but the report:

```
$ substrate-node-probe --node-address wss://rpc.polkadot.io --follow 2 --json 2>/dev/null
{
  "endpoint": "wss://rpc.polkadot.io",
  "ok": true,
  "genesis_hash": "0x91b171bb158e2d3848fa23a9f1c25182fb8e20313b2c1eb49219da7a70ce90c3",
  "genesis_verified": true,
  "rpc_latency_ms": 63,
  "name": "Parity Polkadot",
  "chain": "Polkadot",
  "version": "1.24.0-660acefe665",
  "peers": 65,
  "is_syncing": false,
  "should_have_peers": true,
  "heads_followed": 2
}
```

Two properties worth relying on:

- **A failed run still prints a report** — `"ok": false` with an `error`, plus whatever was gathered before the failure. A probe that goes silent exactly when the node breaks is no use to whatever is parsing it. A genesis mismatch, for instance, still reports the hash the node actually serves, so you needn't parse it back out of the error string.
- **Fields the node did not answer are omitted, not `null`** — `"peers": 0` means an isolated node; a missing `peers` means the node never said. `system_health` is not exposed everywhere, and a refusal costs only its own fields rather than the whole report.

`genesis_verified` is `true` only when `--genesis-hash` was supplied *and* matched; without the flag the hash is reported but nothing about it is proven. `should_have_peers` is what makes `peers: 0` interpretable — it is `false` on a dev chain running alone.

### Why WebSocket

Steps 1–3 don't need it — request/response would work as well over HTTP POST. `--follow` is the part that does: a subscription has the node push frames unprompted for as long as the connection lives, which is what the transport is actually for.

```
$ substrate-node-probe --node-address wss://rpc.polkadot.io --follow 3

INFO  Subscribed with id JfjLVdZ5TGesJIVX
INFO  New head #32263282 parent=0xb2ddbc8a89c37c57b07136cf2c997c8074b0c7de309f19eae00a990c2e3bfe8c
INFO  New head #32263283 parent=0x52aa1f4c48e680c82f9a5838f8e1f16f546e427b03c842e572e4b3225713e766
INFO  New head #32263284 parent=0x0e0c80ba8389731ff05c28a2810a3093d018216b12bbe2fd11cc70982c093397
INFO  Unsubscribed after 3 header(s)
```

### Scope

This talks to a node's **RPC endpoint**, not its peer-to-peer port. Verifying the genesis hash confirms *which chain* you are connected to; it is not peer authentication, which is a libp2p protocol on port 30333 requiring Noise, multistream-select and yamux.

The `HandshakeMessage` struct in `src/scale.rs` is a worked example of SCALE encoding — Substrate's wire codec — kept with a round-trip test. It is deliberately not sent to the node, because an RPC endpoint has no notion of it.

## Layout

| File | Holds |
| --- | --- |
| `src/main.rs` | The CLI flags, and the order the steps run in. |
| `src/rpc.rs` | The transport: opening the WebSocket, request ids, timeouts, reading frames. Knows nothing about chains. |
| `src/probe.rs` | The JSON-RPC calls — genesis hash, identity and health, following heads. |
| `src/report.rs` | The findings, their JSON shape, and the `--require-*` checks. |
| `src/scale.rs` | The SCALE codec example. |

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
| `--follow <COUNT>` | *(none)* | Follow this many new block headers after querying, then unsubscribe and exit. Omit to exit straight after the query. |
| `--require-peers <N>` | *(none)* | Fail unless the node reports at least `N` connected peers. |
| `--require-synced` | *(off)* | Fail if the node is still syncing. |
| `--json` | *(off)* | Print the findings to stdout as one JSON object. Logs stay on stderr. |

Exit code is `0` on success and `1` on any failure, so it works as a health check directly:

```bash
substrate-node-probe --node-address wss://rpc.polkadot.io \
  --require-peers 1 --require-synced || alert "polkadot rpc is unusable"
```

### Requirements

Without `--require-*` the exit code only means *the node answered*. A node can answer every query correctly, on the right chain, and still be useless — connected to nobody, or serving stale state while it catches up. The flags are what make the exit code mean *the node is usable*:

```
ERROR node has 0 peer(s), --require-peers demands 1
ERROR node is still syncing
```

A requirement that **cannot be evaluated fails**. If the node will not say how many peers it has — `system_health` is not exposed everywhere — then `--require-peers` has not been met, and the probe says so rather than passing. Treating silence as success is how a health check comes to certify a broken node.

Requirements are judged before `--follow`, so a node that has already failed is not watched for another block.

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
cargo test -- --ignored         # plus the live-network probe (see below)
cargo clippy --all-targets      # lints
cargo fmt --check               # formatting
```

Tests run against an in-process mock WebSocket node, so they need no network and no running Substrate node. Timeouts are injectable, so the timeout paths are covered with short real durations and the whole suite finishes in well under a second.

### The live test

The mock node answers whatever the test file tells it to, so it can only prove the client is self-consistent — it would keep passing if an RPC method were renamed, a result shape changed, or the `wss://` and rustls path broke. One `#[ignore]`d test runs the whole probe against `wss://rpc.polkadot.io` and asserts on the real answers.

It is excluded from `cargo test` and never runs on a pull request, because it depends on a third party being up: a PR failing because someone else's endpoint blinked is the false alarm that teaches you to ignore CI. [`live-probe.yml`](.github/workflows/live-probe.yml) runs it weekly instead, retrying twice before reporting — one dropped connection is noise, three in a row is a signal.

## Notes

- TLS uses `rustls` with the `ring` provider, so the build needs no OpenSSL system libraries.
- Both the connect and per-response paths are bounded by a 10-second timeout; a node that accepts the socket and goes silent produces an error rather than a hang. Waiting for a pushed block header uses a separate, much longer bound (120s), since that waits on the chain's block time rather than on the node being responsive.
