# 3. Reading the chain

A node answers questions over plain HTTP with JSON. No key, no account, no client library:
`curl` is enough, and so is `fetch` in a browser.

The examples use the private node from chapter 2. Replace the address with one of yours.

```bash
N=http://127.0.0.1:28556
A=imntest:qqzh3r96a2v3454gxue2a59l85nvrup6mllm6xmr84jenq7e2fd6ccalehw
```

## Is the node alive, and which network is it on?

```bash
curl $N/api/v1/info
```

```json
{ "network": "devnet", "genesis_hash": "75d567cd…", "total_blocks": 343,
  "virtual_blue_score": 343, "tips": ["4eb9f2bf…"], "current_bits": "0x1e47628a",
  "current_block_reward_imn": 5.0, "final_confirmations": 60, "finality_depth": 8640,
  "network_alert": false, "mempool_size": 0 }
```

Check `network` and `genesis_hash` before trusting anything else a node says: they tell you
which chain you are talking to. `virtual_blue_score` is the height that confirmations are
counted from.

## What does an address hold?

```bash
curl $N/api/v1/address/$A/balance
```

```json
{ "address": "imntest:qqzh3r…", "balance_imn": 2.5, "balance_atoms": 250000000 }
```

Use `balance_atoms`. The `_imn` fields are for display.

## Which coins can it spend?

```bash
curl $N/api/v1/address/$A/utxos
```

```json
[ { "transaction_id": "491e5f34…", "index": 0, "value_atoms": 250000000, "value_imn": 2.5,
    "spendable": true, "confirmations": 85 } ]
```

This is what a wallet needs before it can build a payment. `spendable` is false only for a
mining reward that has not matured.

## What has happened at an address?

```bash
curl "$N/api/v1/address/$A/history?limit=20"
```

```json
{ "items": [ { "tx_id": "491e5f34…", "kind": "received", "received_atoms": 250000000,
               "sent_atoms": 0, "net_atoms": 250000000, "confirmations": 8,
               "timestamp_ms": 1791532658922 } ],
  "next": null }
```

Newest first. `kind` is `received`, `sent` or `reward`, and each row is the net effect of one
transaction on the address, so a payment with change is a single `sent` row. When `next` is
not null, pass it back as `before=` for the following page. `limit` is at most 500.

## What happened to a payment?

```bash
curl $N/api/v1/tx/491e5f3437bf119045f09ed68f16e03e28cfdae5e7a0829d1b333ad7611b0c7b
```

```json
{ "tx_id": "491e5f34…", "status": "confirmed", "block_hash": "71051b08…", "confirmations": 8,
  "inputs": ["000e5fe6…:0"],
  "outputs": [ { "address": "imntest:qqzh3r…", "amount_atoms": 250000000, "amount_imn": 2.5 },
               { "address": "imntest:qr48j0…", "amount_atoms": 249990000, "amount_imn": 2.4999 } ],
  "invoice_id": "order-1001", "size_bytes": 263 }
```

`status` is `pending` (a node has it, no block yet), `confirmed` or `not_found`. The second
output here is the sender's change.

## Has an invoice been paid?

```bash
curl "$N/api/v1/invoice/order-1001?address=$A"
```

```json
{ "invoice_id": "order-1001",
  "payments": [ { "tx_id": "491e5f34…", "amount_atoms": 250000000, "confirmations": 3, "level": "included" } ],
  "seen_atoms": 250000000, "included_atoms": 250000000, "final_atoms": 0,
  "final_confirmations": 60, "network_alert": false }
```

The three totals are the levels from chapter 1, and each includes the ones after it. Compare
the one you care about with the amount you asked for. Chapter 5 builds on this call.

## Blocks

```bash
curl "$N/api/v1/blocks?limit=5"      # the newest blocks
curl $N/api/v1/block/<hash>          # one block
```

```json
{ "hash": "03b2a50e…", "parents": ["bd890aab…"], "selected_parent": "bd890aab…",
  "blue_score": 453, "daa_score": 453, "timestamp_ms": 1791532737976, "bits": "0x1e08160b",
  "mergeset_blues": ["bd890aab…"], "mergeset_reds": [],
  "transaction_ids": ["d830c782…"], "miner_address": "imntest:qr48j0…", "reward_imn": 5.0 }
```

`parents` has more than one entry when blocks were found side by side.

## Network figures

```bash
curl $N/api/v1/stats
```

Difficulty, estimated hashrate, average block time, supply and the latest payments, computed
over the last 144 blocks. This is what the explorer's front page shows.

## Being told instead of asking

Open a WebSocket to `/api/v1/ws/address/<address>` and the node sends a message when a payment
to that address reaches it (`"event": "payment_seen"`) and when the balance changes
(`"event": "payment_received"`).

Treat a message as a prompt to ask again over HTTP, not as the answer. A connection can drop
and miss one; the HTTP answers are the record.

## Things that catch people out

- **Escape the colon when a tool requires it.** `imntest%3Aqqzh…` and `imntest:qqzh…` are
  both accepted in a path; some HTTP libraries insist on the first.
- **A malformed address is a `400`**, not an empty result.
- **A pruned node forgets old payments.** A node started with `--prune` keeps balances and
  recent history but answers `not_found` for payments older than about 36 hours. Run an
  unpruned node if you need to look up old payments.
- **The public website passes these calls through** at `https://internetmoneynetwork.org/api/v1/…`,
  which is convenient for trying things. For anything that handles money, ask your own node.

Next: [keys and sending](04-keys-and-sending.md).
