# Internet Money (`IMN`) - Master Production & Merchant SDK Roadmap

This roadmap defines the step-by-step engineering plan to build Internet Money into a production-grade Layer-1 payment network with plug-and-play merchant integration.

---

## 🏗️ Phase 1: Core Protocol Hardening (The Foundation)
*Goal: Ensure the blockchain ledger is persistent, secure, and has working addresses and transactions.*

1. **Bech32 Address & Cryptography Engine (`imoney-core`)**
   - Standardized checksummed addresses: `imn:q<hash>` (Classical) and `imn:pq1<hash>` (Post-Quantum).
   - Keypair generation (Private Key -> Public Key -> Address).
   - Transaction signing & verification (Schnorr / Falcon-512).
2. **Persistent ACID On-Disk Storage (`imoney-node`)**
   - Embedded pure-Rust ACID storage (`redb`) in `imoney-node`.
   - Saves block headers, DAG parent relations, blue scores, and UTXO state to `--data-dir`.
   - Node recovers instantly on restart with zero data corruption.
3. **UTXO Ledger & Mempool**
   - Coinbase transaction (miners receive real 5 IM block rewards to their address).
   - Transfer transactions (spending inputs, creating new outputs, fee validation).
   - Mempool to hold pending transactions before block inclusion.

---

## 🌐 Phase 2: The Merchant Indexer API
*Goal: Expose ultra-fast REST & WebSocket endpoints so external apps can query balances and monitor payments.*

1. **Lightweight Indexer Endpoints (`imoney-node`)**
   - `GET /api/v1/address/:addr/balance` - Instant balance lookup.
   - `GET /api/v1/address/:addr/utxos` - Query spendable coins.
   - `POST /api/v1/tx/broadcast` - Broadcast signed transactions to the DAG.
   - `GET /api/v1/tx/:txid` - Query transaction confirmation status.
2. **Real-Time Payment WebSockets**
   - `WS /api/v1/ws/address/:addr` - Push notification fired within milliseconds when a transaction paying that address enters the blockDAG.

---

## 🔌 Phase 3: The Plug-and-Play Developer SDK (`imoney-sdk`)
*Goal: Allow any web developer to add Internet Money payments in 2 lines of code.*

1. **Universal TypeScript / JavaScript SDK (`imoney-sdk`)**
   - Usable in Node.js, React, Vue, Next.js, and vanilla HTML via CDN (`<script src="imoney.js">`).
   - Simple API:
     ```javascript
     const im = new InternetMoney({ nodeUrl: "http://localhost:18556" });
     const order = await im.createPayment({ to: "imn:q...", amount: 10.5, orderId: "INV-101" });
     order.onConfirmed(() => console.log("Payment Confirmed in 5s!"));
     ```
2. **Drop-In Embeddable Checkout Modal (`imoney-checkout.js`)**
   - A single HTML button tag that opens a responsive popup modal with:
     - Order total (IMN + local fiat conversion).
     - QR code for mobile scanning.
     - Live 5-second countdown & instant green checkmark on settlement.

---

## 🛒 Phase 4: CMS Plugins & E-Commerce (WordPress / WooCommerce)
*Goal: Zero-code integration for 40%+ of all internet web stores.*

1. **Official WooCommerce Plugin (`imoney-payments-for-woocommerce`)**
   - Standard WordPress `.zip` plugin.
   - Setup: Merchant pastes their `imn:q...` payout address in WordPress settings.
   - Checkout Flow:
     1. Customer chooses "Pay with Internet Money (0% fees)".
     2. Popup displays QR code.
     3. 5-second confirmation marks WooCommerce order as "Completed / Paid".
     4. 100% Non-Custodial: Funds go directly into the merchant's private wallet.
2. **Generic Webhook & Shopify Bridge**
   - Webhook triggers for non-WordPress e-commerce platforms.

---

## 🚀 Execution Order & Status
- [x] **Step 1: Core Protocol Hardening** (Bech32 address format, Ed25519 Schnorr signing, pure-Rust `redb` ACID storage engine, UTXO ledger).
- [x] **Step 2: Payment Indexer & Real-Time Push Engine** (Balance/UTXO lookups, `POST /api/v1/tx/broadcast`, `GET /api/v1/tx/:txid`, WebSocket `/api/v1/ws/address/:addr`).
- [x] **Step 3: Developer SDK & Checkout Modal** (`@imoney/sdk` for Node.js + browser CDN bundle `imoney.js`, reactive payment modal with QR code and live confirmation).
- [x] **Step 4: WordPress / WooCommerce 1-Click Gateway** (`plugins/imoney-payments-for-woocommerce.zip` packaged with non-custodial payout address configuration).
- [x] **Step 5: End-to-End Payment Demo** (`examples/e2e-payment-demo/index.html` illustrating 2-line code web store checkout).
- [x] **Step 6: P2P Peer Gossip Networking** (Pure-async TCP framing, bidirectional Handshake, `GetTips`, block sync, mempool transaction propagation, `/api/v1/peers`).
- [ ] **Step 7: Seed Node Deployment & Testnet Public Launch** (Deploying local network to cloud seed nodes under `internetmoneynetwork.org`).


