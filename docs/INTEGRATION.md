# Integrating Internet Money

For exchanges, wallet developers and mining pools. Everything here is the test network; the
protocol can still change before a main network exists.

All examples talk to a node's HTTP API (default `http://127.0.0.1:18556`). Run your own node:
see [RUNNING-A-NODE.md](RUNNING-A-NODE.md). Amounts in the API are in **atoms**
(1 IMN = 100,000,000 atoms) unless a field name ends in `_imn`. Use the atom fields for money
arithmetic; the `_imn` fields are floating-point conveniences for display.

## The basics

| | |
| :--- | :--- |
| Model | UTXO. A transaction spends whole outputs and creates new ones. |
| Signatures | Ed25519. |
| Address | `imn:q…` (main network) or `imntest:q…` (test network). Bech32 with `:` in place of the `1` separator; the payload is a type byte and the 32-byte Blake3 hash of the public key. |
| Block time | 5 seconds. Blocks form a DAG ordered by GHOSTDAG, not a single chain. |
| Confirmations | 0 while pending, 1 once accepted into the ledger, then one more per blue block added on top. |
| Finality | Blocks more than 12 hours deep are never reorganised. |
| Reward maturity | Mining rewards are spendable after 20 confirmations (test network). |
| Minimum fee | 10 atoms per byte of transaction (a simple payment is about 280 bytes). |

One thing differs from single-chain coins: **being in a block is not the same as being
accepted.** Two parallel blocks may carry conflicting payments, and only one is accepted. Never
credit a deposit because a transaction appears in a block's list. Credit it from the
confirmation count, as described below.

## Exchanges and payment processors

### Receiving deposits

Two ways to tell users' deposits apart. Either works; the second needs only one address.

**A. One address per user.** Generate a key per user and poll its coins:

```
GET /api/v1/address/{address}/utxos
→ [ { "transaction_id": "…", "index": 0, "value_atoms": 150000000, "spendable": true, "confirmations": 12 } ]
```

Credit a coin once its `confirmations` reaches your threshold. A coin that disappears from the
list before then was reorganised away or spent.

**B. One address, an invoice ID per user or per deposit.** The sender's wallet signs the ID into
the payment (it plays the role a memo or destination tag plays elsewhere, but cannot be altered
after signing). An ID is 1–64 characters from `A-Z a-z 0-9 - _ .`.

```
GET /api/v1/invoice/{id}?address={your address}
→ { "payments": [ { "tx_id": "…", "amount_atoms": 150000000, "confirmations": 12, "level": "included" } ],
    "seen_atoms": 150000000, "included_atoms": 150000000, "final_atoms": 0,
    "final_confirmations": 60, "network_alert": false }
```

`final_atoms` counts payments with at least `final_confirmations` confirmations while the node
is not reporting `network_alert`. Set the threshold with the node's `--final-confirmations`
option to match the value you are protecting.

Give users the payment request form, which wallets parse:
`imntest:q…?amount=1.5&invoice=USER-48213`

For push instead of polling, `WS /api/v1/ws/address/{address}` sends `payment_seen` when a
payment reaches the node and `payment_received` when the balance changes. Treat these as a hint
to poll; the REST answers are the record.

### How many confirmations

Reversing a payment costs an attacker the hashpower to out-mine the network for that many
blocks. On a young network that cost is low, so use a deep threshold for large deposits. Watch
two fields in `GET /api/v1/info`:

- `network_alert`: true during a finality conflict or for 30 minutes after a reorganisation of
  3 or more blocks. Pause crediting while it is set.
- `finality_conflict`: true when a heavier chain exists that this node refuses. The network may
  have split; stop deposits and withdrawals and investigate.

### Sending withdrawals

Keys never go to the node. Build and sign locally, then submit the signed transaction:

```
POST /api/v1/tx/broadcast      { "transaction": { … } }
→ { "success": true, "tx_id": "…" }      or      { "success": false, "error": "…" }
GET  /api/v1/tx/{tx_id}
→ { "status": "pending" | "confirmed" | "not_found", "confirmations": 3, "inputs": [ "…:0" ],
    "outputs": [ { "address": "imntest:q…", "amount_atoms": 150000000 } ], "invoice_id": null, … }
```

To build transactions, use the project's own code so the encoding is exactly right:

- **Rust:** the `imoney-core` crate: `Transaction::build_invoice_payment` and
  `Transaction::build_consolidation`.
- **JavaScript / browser / Node.js:** the WebAssembly build in `crates/imoney-wasm`
  (`build_payment`, `build_consolidation`, `keys_from_mnemonic`). Pass a fee of 0 to have it set
  from the transaction's size.

A `not_found` status for a withdrawal you submitted means it was dropped or reorganised away
and can be sent again; its inputs are free once they reappear in your coin list.

Consolidate regularly. Every deposit is a separate coin, and a withdrawal that spends hundreds of
small coins is large and costs more. `build_consolidation` merges up to 300 coins into one.

### Listing checklist

| Need | Where |
| :--- | :--- |
| Node status and height | `GET /api/v1/info` (`virtual_blue_score` is the height) |
| Supply, hashrate, difficulty | `GET /api/v1/stats` |
| Validate an address | decode it with `imoney-core` or `is_valid_address` in the WebAssembly build |
| Block by hash | `GET /api/v1/block/{hash}` |
| Recent blocks | `GET /api/v1/blocks?limit=N` |
| Explorer | served by any node at `/explorer` |

## Wallets

A wallet needs four things: keys, the user's coins, a signed transaction, and somewhere to send it.

1. **Keys.** A 12-word BIP-39 phrase. The key is
   `Blake3-derive-key("IMN 2026 wallet key 0", BIP-39 seed with empty passphrase)`, used as an
   Ed25519 private key. This derivation is specific to Internet Money; other coins' paths do
   not apply.
2. **Coins.** `GET /api/v1/address/{address}/utxos`. Spend only those with `spendable: true`.
3. **Signing.** The canonical transaction encoding and the signing hash are defined in
   `crates/imoney-core/src/transaction.rs` and summarised in SPECIFICATION.md. The signing hash
   commits to the network, so a testnet signature is invalid on the main network. Reuse the
   Rust crate or the WebAssembly build unless you have a reason not to.
4. **Sending.** `POST /api/v1/tx/broadcast`.

Two things users will thank you for:

- **Parse payment requests** (`address?amount=…&invoice=…`) and attach the invoice ID. Without it
  a shop cannot match the payment to an order.
- **Name the node's service address.** `GET /api/v1/info` returns `service_address`. Naming it in
  a payment gives that node half of the fee at no extra cost to the user. A user running their
  own node gets that half back.

The reference wallet is `apps/imoney-wallet` (one HTML file plus the WebAssembly module).

## Mining pools and miner software

### Proof of work

Money Printer is FishHash with a network-specific seed. Nothing else about the algorithm changes.

- **Seed:** `Blake3-derive-key("IMN 2026 Money Printer seed", genesis block hash)`, 32 bytes.
  The node reports the genesis hash in `GET /api/v1/info`. The seed never changes, so the
  dataset is built once.
- **Hash input:** 40 bytes: the block's 32-byte pre-proof-of-work hash, then the 8-byte
  little-endian nonce.
- **Valid when:** the 32-byte FishHash output, read as a big-endian number, is at most the target.
- **Sizes:** the main network will use the FishHash specification's sizes (75 MB light cache,
  4.6 GB dataset). The current test network uses small sizes so a CPU can mine; a FishHash GPU
  kernel does not apply to it as is.

The reference implementation is `crates/imoney-pow/src/fishhash.rs`, checked byte for byte
against Iron Fish's implementation at the specification's sizes.

### Getting work and submitting blocks

```
GET  /api/v1/mining/template?address={payout address}
→ { "block": { "header": { …, "nonce": 0 }, "transactions": [ … ] }, "pre_pow_hash": [ … ], "target_hex": "…" }
POST /api/v1/mining/submit     { "block": { …the template's block with header.nonce set… } }
→ { "success": true, "block_hash": "…" }
```

Fetch a new template at least every second or two: parallel blocks are normal, and a template
is stale as soon as the tips change. A stale block is rejected with a reason; that is routine.
If the node was started with `--rpc-token`, send it as `Authorization: Bearer <token>` on submit.

The reference miner is `crates/imoney-miner` (`imoney-miner --node <url> --address <address>`).

### What a pool has to build

The node speaks HTTP, not Stratum. A pool needs a bridge that fetches templates, hands miners
the pre-proof-of-work hash and target with a nonce range, checks shares with the light cache,
and submits full blocks. There is no reference Stratum bridge yet.

Rewards: the coinbase names one payout script. The ledger pays that script the block subsidy
plus the miner's share of fees when the block is merged as blue. A red block earns nothing, so
a pool should expect a small fraction of found blocks to pay zero.

## What is not here yet

Being direct about gaps saves everyone time:

- **No address history.** A node can list an address's current coins, not its past transactions.
  Keep your own record of deposits and withdrawals.
- **No Stratum bridge and no GPU miner.**
- **No hardware-wallet support and no multi-signature addresses.** The address format reserves
  a type for scripts; nothing implements it.
- **No client libraries beyond Rust and the WebAssembly build.**
- **A pruned node forgets** transactions and invoices older than 36 hours. Run an unpruned node
  for exchange or pool work.
