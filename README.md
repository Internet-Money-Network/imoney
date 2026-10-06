<p align="center">
  <img src="https://raw.githubusercontent.com/Internet-Money-Network/imoney/main/assets/branding/imoney-logo-banner.png" alt="Internet Money Logo Banner" width="800">
</p>

<p align="center">
  <b>A fast, ASIC-resistant BlockDAG Layer-1 cryptocurrency for peer-to-peer commerce.</b><br>
  <sub>Zero Premine &bull; Zero Dev Fee &bull; 5-Second Settlement &bull; Pure-Rust redb Storage &bull; Native Merchant SDK</sub>
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

- **5-Second Settlement (0.2 BPS):** Combines the GHOSTDAG ($k=8$) parallel block consensus model with 5,000 ms block times. Honest concurrent blocks merge into a single directed acyclic graph without chain splits or high orphaning rates.
- **ASIC-Resistant "Money Printer" PoW:** Memory-hard Blake3 + Keccak256 algorithm requiring high-bandwidth pseudo-random lookups across memory (FishHash derivative). Prevents custom ASIC dominance by forcing miners to utilize retail GDDR6/HBM memory bandwidth.
- **Sub-1% Long-Term Inflation Floor:** Strict 4-year halving cycle starting at 5.0 IM/block, tapering down to a permanent 0.3125 IM (~1% annual inflation) security subsidy floor to ensure permanent miner incentive without exorbitant user fees.
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
| **Proof-of-Work Algorithm** | **Money Printer** (Memory-hard GPU-friendly PoW) |
| **Launch Block Reward** | **5 IM** per block (500,000,000 atoms) |
| **Halving Cycle** | Every 4 years (25,228,800 blocks) |
| **Tail Emission Floor** | **0.3125 IM** per block (~1.97M IM / year forever) |
| **Decimal Precision** | 8 decimal places (1 IM = 100,000,000 atoms) |
| **Initial Year-1 Emission** | ~31,536,000 IM |

---

## 📊 Emission & Halving Schedule

```text
Era 0 (Years 0 - 4):    5.00000000 IM / block  -->  126,144,000 IM total
Era 1 (Years 4 - 8):    2.50000000 IM / block  -->   63,072,000 IM total
Era 2 (Years 8 - 12):   1.25000000 IM / block  -->   31,536,000 IM total
Era 3 (Years 12 - 16):  0.62500000 IM / block  -->   15,768,000 IM total
Era 4+ (Year 16+):      0.31250000 IM / block  -->   Permanent Floor (~1% inflation tail)
```

---

## Workspace Architecture

The codebase is structured as a modular mono-repository:

- **Core Protocol & Node (`crates/`):**
  - [`crates/imoney-core`](crates/imoney-core): Bech32 addresses (`imn:q...`), Ed25519 Schnorr signing, transaction verification.
  - [`crates/imoney-pow`](crates/imoney-pow): Memory-hard "Money Printer" algorithm (Blake3 + Keccak256 memory bandwidth lookups).
  - [`crates/imoney-emission`](crates/imoney-emission): 4-year halving curve starting at 5 IM/block down to 0.3125 IM permanent floor.
  - [`crates/imoney-consensus`](crates/imoney-consensus): GHOSTDAG ordering rules ($k=8$), blue scores, and rolling DAA window.
  - [`crates/imoney-node`](crates/imoney-node): Full node daemon with embedded pure-Rust `redb` ACID storage, REST/WebSocket API, and TCP P2P gossip sync.
  - [`crates/imoney-miner`](crates/imoney-miner): Reference multi-threaded miner CLI and benchmark.
- **Ecosystem Apps (`apps/`):**
  - [`apps/imoney-explorer`](apps/imoney-explorer): Real-time BlockDAG visualizer, network metrics, and address/tx search.
  - [`apps/imoney-wallet`](apps/imoney-wallet): Non-custodial web & desktop wallet with client-side key management.
  - [`apps/imoney-website`](apps/imoney-website): Official portal website for `internetmoneynetwork.org`.
- **Developer Tools & Plugins:**
  - [`packages/imoney-sdk`](packages/imoney-sdk): Universal TypeScript/JavaScript SDK and CDN-ready bundle (`imoney.js`).
  - [`plugins/imoney-payments-for-woocommerce`](plugins/imoney-payments-for-woocommerce): 1-click zero-fee WooCommerce payment gateway.


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

### Run Benchmark & Test Mining
```bash
# Run hashrate benchmark
cargo run --bin imoney-miner -- --benchmark

# Mine a sample test blockDAG header
cargo run --bin imoney-miner -- --mine-test
```

---

## 📜 License
Licensed under either of [MIT](LICENSE) or [Apache-2.0](LICENSE-APACHE) at your option.
