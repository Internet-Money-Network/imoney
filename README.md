# Internet Money (`IMN`)

> **Permissionless, ASIC-resistant, 5-second blockDAG digital cash for the modern economy.**

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)

---

## 💡 Overview

**Internet Money (`IMN`)** is a decentralized cryptocurrency designed from the ground up to fulfill the original vision of electronic peer-to-peer cash:
- **Instant Finality:** 1 block every 5 seconds (0.2 BPS) on an asynchronous **GHOSTDAG** (blockDAG) consensus engine.
- **True ASIC Resistance:** Powered by the **Money Printer** algorithm, a memory-hard Proof-of-Work engine designed specifically to saturate consumer gaming GPU memory bandwidth and keep mining decentralized among everyday users.
- **Fair Launch & Sound Money:** Zero premine, zero founder tax, zero venture capital. Follows a 4-year halving cycle with a permanent 0.3125 IM / block floor (~1% perpetual tail inflation) guaranteeing infinite network security without fee-gating normal users.
- **Lightweight Nodes:** By pacing blocks at 5 seconds ($k=8$), daily block header volume is reduced by **50x compared to Kaspa**, allowing full nodes to run effortlessly on laptops, modest PCs, or Raspberry Pis.

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

## 🗂️ Workspace Architecture

The codebase is organized as a modular Rust workspace:

- [`crates/imoney-core`](crates/imoney-core): Primitive types (`BlockHeader`, `Transaction`, `Hash`, consensus constants).
- [`crates/imoney-pow`](crates/imoney-pow): The **Money Printer** memory-hard Proof-of-Work engine and CPU/GPU validation functions.
- [`crates/imoney-emission`](crates/imoney-emission): Exact integer-based emission curve, halving calculations, and supply models.
- [`crates/imoney-consensus`](crates/imoney-consensus): GHOSTDAG ordering rules, blue/red set coloring, and Difficulty Adjustment Algorithm (DAA).
- [`crates/imoney-miner`](crates/imoney-miner): Reference multi-threaded miner CLI and hashrate benchmark.

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
