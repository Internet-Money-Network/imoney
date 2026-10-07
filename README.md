<p align="center">
  <img src="https://raw.githubusercontent.com/Internet-Money-Network/imoney/main/assets/branding/imoney-logo-banner.png" alt="Internet Money Logo Banner" width="800">
</p>

<p align="center">
  <b>A proof-of-work BlockDAG Layer-1 cryptocurrency built for payments.</b><br>
  <sub>Zero Premine &bull; Zero Dev Fee &bull; ~5-Second Block Inclusion &bull; Pure-Rust redb Storage &bull; Native Merchant SDK</sub>
</p>


<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg" alt="License"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.75%2B-orange.svg" alt="Rust"></a>
  <a href="apps/imoney-explorer/index.html"><img src="https://img.shields.io/badge/explorer-testnet--1-green.svg" alt="Explorer"></a>
  <a href="packages/imoney-sdk"><img src="https://img.shields.io/badge/sdk-%40imoney%2Fsdk-blueviolet.svg" alt="SDK"></a>
</p>

---

## Overview

**Internet Money (`IMN`)** is an open-source, permissionless Layer-1 proof-of-work cryptocurrency designed for real-world merchant checkout and decentralized digital cash.

- **~5-Second Block Inclusion (0.2 BPS):** Combines the GHOSTDAG ($k=8$) parallel block consensus model with 5,000 ms block times. Honest concurrent blocks merge into a single directed acyclic graph without chain splits or high orphaning rates.
- **"Money Printer" PoW (FishHash):** The memory-hard FishHash algorithm from Iron Fish, with a seed specific to this network. Each hash makes random reads across a 4.6 GB dataset, which favours commodity GPU memory over custom chips. Nodes verify blocks from a 75 MB cache.
- **Sub-1% Long-Term Inflation Floor:** Strict 4-year halving cycle starting at 5.0 IMN/block, tapering down to a permanent 0.3125 IMN (about 0.83% annual inflation at year 16, declining) security subsidy floor to ensure permanent miner incentive without exorbitant user fees.
- **Lightweight Nodes (~5–8 GB):** Generating only 17,280 blocks/day (50x fewer than 10 BPS chains), pruned full nodes run on standard hardware and modest VPS instances using pure-Rust embedded `redb` ACID persistence.
- **Plug-and-Play Merchant Stack:** Zero-custody developer SDK (`@imoney/sdk`) and 1-click WooCommerce/WordPress plugin supporting sub-second WebSocket payment detection and 5-second customer checkouts.


---

## ⚙️ Key Specifications

| Parameter | Value |
| :--- | :--- |
| **Ticker** | `IMN` |
| **Consensus Engine** | GHOSTDAG (blockDAG) with $k = 8$ |
| **Block Interval** | **5,000 ms (5 seconds)** |
| **Blocks Per Day** | 17,280 |
| **Blocks Per Year** | 6,307,200 |
| **Proof-of-Work Algorithm** | **Money Printer** (FishHash with a network-specific seed) |
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
  - [`crates/imoney-pow`](crates/imoney-pow): "Money Printer" proof of work: a port of FishHash, checked against Iron Fish's reference implementation.
  - [`crates/imoney-emission`](crates/imoney-emission): 4-year halving curve starting at 5 IMN/block down to 0.3125 IMN permanent floor.
  - [`crates/imoney-consensus`](crates/imoney-consensus): GHOSTDAG ordering rules ($k=8$), blue scores, and rolling DAA window.
  - [`crates/imoney-node`](crates/imoney-node): Full node daemon with embedded pure-Rust `redb` ACID storage, REST/WebSocket API, and TCP P2P gossip sync.
  - [`crates/imoney-miner`](crates/imoney-miner): Reference multi-threaded miner CLI and benchmark.
- **Ecosystem Apps (`apps/`):**
  - [`apps/imoney-explorer`](apps/imoney-explorer): Real-time BlockDAG visualizer, network metrics, and address/tx search.
  - [`apps/imoney-wallet`](apps/imoney-wallet): Testnet web wallet. Keys are currently generated and used for signing by your own node; client-side signing is planned.
  - [`apps/imoney-website`](apps/imoney-website): Official portal website for `internetmoneynetwork.org`.
- **Developer Tools & Plugins:**
  - [`packages/imoney-sdk`](packages/imoney-sdk): Universal TypeScript/JavaScript SDK and CDN-ready bundle (`imoney.js`).
  - [`plugins/imoney-payments-for-woocommerce`](plugins/imoney-payments-for-woocommerce): 1-click WooCommerce payment gateway.


---

## 🚀 Quick Start

### Prerequisites
* [Rust](https://rustup.rs/) (version 1.75 or higher)

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

# In another terminal: mine for that node, paying an address you control
cargo run --release --bin imoney-miner -- --node http://127.0.0.1:18556 --address imntest:q...

# Or run a node that mines by itself
cargo run --release --bin imoney-node -- --auto-mine

# Hashrate benchmark
cargo run --release --bin imoney-miner -- --benchmark
```

---

## 📜 License
Licensed under either of [MIT](LICENSE) or [Apache-2.0](LICENSE-APACHE) at your option, with one exception: [`crates/imoney-pow/src/fishhash.rs`](crates/imoney-pow/src/fishhash.rs) is a port of Iron Fish's FishHash implementation and is licensed under the [MPL-2.0](https://mozilla.org/MPL/2.0/).
