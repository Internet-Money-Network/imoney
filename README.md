<p align="center">
  <img src="https://raw.githubusercontent.com/Internet-Money-Network/imoney/main/assets/branding/imoney-logo-banner.png" alt="Internet Money Logo Banner" width="800">
</p>

<p align="center">
  <b>A proof-of-work BlockDAG Layer-1 cryptocurrency built for payments.</b><br>
  <sub>Zero Premine &bull; Zero Dev Fee &bull; ~5-Second Block Inclusion &bull; Pure-Rust redb Storage &bull; Native Merchant SDK</sub>
</p>


<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg" alt="License"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.97-orange.svg" alt="Rust"></a>
  <a href="apps/imoney-explorer/index.html"><img src="https://img.shields.io/badge/explorer-testnet--1-green.svg" alt="Explorer"></a>
  <a href="packages/imoney-sdk"><img src="https://img.shields.io/badge/sdk-%40imoney%2Fsdk-blueviolet.svg" alt="SDK"></a>
</p>

---

## Overview

**Internet Money (`IMN`)** is an open-source, permissionless Layer-1 proof-of-work cryptocurrency designed for real-world merchant checkout and decentralized digital cash.

- **~5-Second Block Inclusion (0.2 BPS):** Combines the GHOSTDAG ($k=8$) parallel block consensus model with 5,000 ms block times. Honest concurrent blocks merge into a single directed acyclic graph instead of being thrown away, and history older than 12 hours is final.
- **Hallmark PoW (FishHash):** The memory-hard FishHash algorithm from Iron Fish, with a seed specific to this network. Each hash makes random reads across a 4.6 GB dataset, which favours commodity GPU memory over custom chips. Nodes verify blocks from a 75 MB cache.
- **Sub-1% Long-Term Inflation Floor:** Strict 4-year halving cycle starting at 5.0 IMN/block, tapering down to a permanent 0.3125 IMN (about 0.83% annual inflation at year 16, declining) security subsidy floor to ensure permanent miner incentive without exorbitant user fees.
- **Small Nodes:** One block every five seconds, a 100 KB block limit and optional pruning keep a full verifying node within reach of a small VPS, using pure-Rust embedded `redb` storage.
- **Merchant Stack:** Every order gets an invoice number that the payer's wallet signs into the payment. The SDK (`@imoney/sdk`) and WooCommerce plugin report it as seen (under a second), in a block (about 5 seconds) or final, confirmed by the merchant's own node.


---

## ⚙️ Key Specifications

| Parameter | Value |
| :--- | :--- |
| **Ticker** | `IMN` |
| **Consensus Engine** | GHOSTDAG (blockDAG) with $k = 8$ |
| **Block Interval** | **5,000 ms (5 seconds)** |
| **Blocks Per Day** | 17,280 |
| **Blocks Per Year** | 6,307,200 |
| **Proof-of-Work Algorithm** | **Hallmark** (FishHash with a network-specific seed) |
| **Launch Block Reward** | **5 IMN** per block (500,000,000 atoms) |
| **Halving Cycle** | Every 4 years (25,228,800 blocks) |
| **Tail Emission Floor** | **0.3125 IMN** per block (~1.97M IMN / year forever) |
| **Decimal Precision** | 8 decimal places (1 IMN = 100,000,000 atoms) |
| **Initial Year-1 Emission** | ~31,536,000 IMN |

---

## 📊 Emission & Halving Schedule

```text
Era 0 (Years 0 - 4):    5.00000000 IMN / block  -->  126,144,000 IMN total
Era 1 (Years 4 - 8):    2.50000000 IMN / block  -->   63,072,000 IMN total
Era 2 (Years 8 - 12):   1.25000000 IMN / block  -->   31,536,000 IMN total
Era 3 (Years 12 - 16):  0.62500000 IMN / block  -->   15,768,000 IMN total
Era 4+ (Year 16+):      0.31250000 IMN / block  -->   Permanent Floor (<1% inflation tail)
```

---

## Workspace Architecture

The codebase is structured as a modular mono-repository:

- **Core Protocol & Node (`crates/`):**
  - [`crates/imoney-core`](crates/imoney-core): Addresses (`imn:q...`), Ed25519 signing, blocks and transactions with a canonical binary encoding, merkle roots.
  - [`crates/imoney-pow`](crates/imoney-pow): Hallmark proof of work: a port of FishHash, checked against Iron Fish's reference implementation.
  - [`crates/imoney-emission`](crates/imoney-emission): 4-year halving curve starting at 5 IMN/block down to 0.3125 IMN permanent floor.
  - [`crates/imoney-consensus`](crates/imoney-consensus): GHOSTDAG ordering rules ($k=8$), blue scores, and the difficulty rule.
  - [`crates/imoney-node`](crates/imoney-node): Full node daemon with embedded pure-Rust `redb` ACID storage, REST/WebSocket API, and TCP P2P gossip sync.
  - [`crates/imoney-miner`](crates/imoney-miner): Reference multi-threaded miner CLI and benchmark.
  - [`crates/imoney-gpu-miner`](crates/imoney-gpu-miner): OpenCL GPU miner. `--list-devices` shows the cards; `--benchmark` checks the card against the CPU and measures its hashrate.
  - [`crates/imoney-stratum`](crates/imoney-stratum): Stratum bridge between a node and mining software, and with `--pool` a complete small mining pool. See [docs/STRATUM.md](docs/STRATUM.md).
  - [`crates/imoney-loadtest`](crates/imoney-loadtest): Floods a test node with payments and watchers and reports how it copes. See [docs/LOAD-TESTING.md](docs/LOAD-TESTING.md).
  - [`crates/imoney-wasm`](crates/imoney-wasm): WebAssembly build of key handling and signing, so wallets sign in the browser.
- **Ecosystem Apps (`apps/`):**
  - [`apps/imoney-explorer`](apps/imoney-explorer): Block explorer served by the node at `/explorer`: the live block graph, recent blocks, and block, transaction and address lookup.
  - [`apps/imoney-wallet`](apps/imoney-wallet): Web wallet served by the node at `/wallet`. Keys are created and kept in the browser, with a 12-word recovery phrase; the node only ever receives signed transactions.
  - [`apps/imoney-website`](apps/imoney-website): Official portal website for `internetmoneynetwork.org`.
- **Developer Tools & Plugins:**
  - [`packages/imoney-sdk`](packages/imoney-sdk): TypeScript/JavaScript SDK and browser bundle (`imoney.js`): invoices, payment levels, checkout window, QR codes.
  - [`plugins/imoney-payments-for-woocommerce`](plugins/imoney-payments-for-woocommerce): WooCommerce gateway. Each order gets an invoice number and the store's server confirms payment with the merchant's node.


---

## 🚀 Quick Start

### Prerequisites
* [Rust](https://rustup.rs/). The version is pinned in `rust-toolchain.toml` and installed automatically.

### Build
```bash
git clone https://github.com/Internet-Money-Network/imoney.git
cd imoney
cargo build --release
```

### Run a Node and Mine
```bash
# Start a node (wallet and dashboard at http://127.0.0.1:18556)
cargo run --release --bin imoney-node

# In another terminal: mine for that node with a graphics card, paying an address you control
cargo run --release --bin imoney-gpu-miner -- --node http://127.0.0.1:18556 --address imntest:q...

# Check the card against the CPU and measure its hashrate
cargo run --release --bin imoney-gpu-miner -- --benchmark

# For development: a private network with a small dataset, mined by the node's own CPU
cargo run --release --bin imoney-node -- --devnet mytest --auto-mine
```

---

See [docs/RUNNING-A-NODE.md](docs/RUNNING-A-NODE.md) for options, seed nodes, pruning and running a node for a shop.
Exchanges, wallet developers and mining pools: see [docs/INTEGRATION.md](docs/INTEGRATION.md).

### Rebuilding the browser pieces
The wallet's signing module and the SDK bundles are committed, so the node builds without extra tools. To regenerate them after changing `crates/imoney-wasm` or `packages/imoney-sdk`:
```bash
# Signing module (needs: rustup target add wasm32-unknown-unknown, cargo install wasm-bindgen-cli)
cargo build -p imoney-wasm --target wasm32-unknown-unknown --release
wasm-bindgen --target web --no-typescript --out-dir apps/imoney-wallet/pkg target/wasm32-unknown-unknown/release/imoney_wasm.wasm

# SDK bundles, and the copy shipped in the WooCommerce plugin
cd packages/imoney-sdk && npm install && npm test
```

---

## 📜 License
Licensed under either of [MIT](LICENSE) or [Apache-2.0](LICENSE-APACHE) at your option, with one exception: [`crates/imoney-pow/src/fishhash.rs`](crates/imoney-pow/src/fishhash.rs) is a port of Iron Fish's FishHash implementation and is licensed under the [MPL-2.0](https://mozilla.org/MPL/2.0/).

The licences cover the code. The Internet Money name and logo are covered by [TRADEMARKS.md](TRADEMARKS.md): forks are welcome, but a different network needs a different name.
