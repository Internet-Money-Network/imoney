# Stratum bridge

`imoney-stratum` lets mining software connect to an Internet Money node over a plain socket
instead of the node's HTTP API. It is what a pool, or anyone writing a miner, talks to.

```bash
imoney-stratum --node http://127.0.0.1:18556 --bind 0.0.0.0:18557
imoney-gpu-miner --stratum 127.0.0.1:18557 --address imntest:q...
```

## What it does and does not do

- Each miner logs in with a payout address and mines blocks that pay **that address** directly.
  The bridge holds no coins, keeps no balances and pays nobody.
- It checks every share with the light cache, steers each miner towards one share every few
  seconds, and submits shares that are also blocks to the node.
- A pool is this plus accounting: log in miners under the pool's own address, count the shares
  each sends, and pay them from the pool's wallet. That accounting is not included.
- It is a reference. It uses one thread per miner, accepts 256 miners unless told otherwise, and
  has no TLS; put it behind a proxy to expose it publicly.

| Option | Default | Meaning |
| :--- | :--- | :--- |
| `--node` | `http://127.0.0.1:18556` | The node to get work from |
| `--bind` | `0.0.0.0:18557` | Where miners connect |
| `--rpc-token` | none | The node's token, if it has one |
| `--share-interval` | `5` | Seconds between shares each miner is steered towards |
| `--poll-ms` | `500` | How often the node is asked for new work |
| `--max-miners` | `256` | Connections accepted at once |
| `--pow-size` | `testnet` | Dataset size of the network: `testnet` or `mainnet` |

## Protocol

JSON objects, one per line, in the style of Bitcoin's Stratum. A request has `id`, `method` and
`params`; the reply has the same `id`, a `result`, and `error` (`null` or
`[code, message, null]`). Messages from the server that are not replies have `"id": null`.

### 1. Subscribe

```json
→ {"id": 1, "method": "mining.subscribe", "params": ["my-miner/1.0"]}
← {"id": 1, "error": null, "result": [
     [["mining.notify", "1"]],
     "00a1",
     6,
     {"genesis_hash": "…", "light_cache_items": 16411, "dataset_items": 262147}
   ]}
```

- `"00a1"` is this connection's **extranonce**: the first two bytes of every nonce it may use.
  `6` is how many bytes are left for the miner to vary.
- The fourth item describes the network. The proof-of-work seed is
  `Blake3-derive-key("IMN 2026 Money Printer seed", genesis hash)`, and the two sizes are the
  FishHash light cache and dataset sizes in items. Build the dataset from these, not from
  constants: the test network's sizes are not the FishHash specification's.

### 2. Log in

```json
→ {"id": 2, "method": "mining.authorize", "params": ["imntest:q…", "x"]}
← {"id": 2, "result": true, "error": null}
```

The login is the payout address, optionally followed by `.` and a worker name. The second
parameter is ignored.

### 3. Work

```json
← {"id": null, "method": "mining.set_target", "params": ["0000003f…"]}
← {"id": null, "method": "mining.notify", "params": ["2f", "9c41…", true]}
```

- `mining.set_target`: the share target, 32 bytes as 64 hexadecimal digits. A hash counts when,
  read as a big-endian number, it is at most this. It applies to work sent after it and is sent
  again whenever it changes.
- `mining.notify`: job id, the block's 32-byte pre-proof-of-work hash, and whether earlier jobs
  should be dropped (always `true`).

The hash input is 40 bytes: the pre-proof-of-work hash, then the nonce as 8 bytes
**little-endian**. See [INTEGRATION.md](INTEGRATION.md#proof-of-work).

### 4. Submit

```json
→ {"id": 3, "method": "mining.submit", "params": ["imntest:q…", "2f", "00a1000000c0ffee"]}
← {"id": 3, "result": true, "error": null}
```

The nonce is 16 hexadecimal digits, most significant first, and must start with the
connection's extranonce. (As a number it is the same value that goes into the hash input in
little-endian byte order.)

| Error code | Meaning |
| :--- | :--- |
| 20 | Malformed request, unknown method, or a nonce outside this connection's range |
| 21 | Stale: the job is no longer one of the last 8 for this address |
| 22 | Duplicate share |
| 23 | The hash is above the share target |
| 24 | Not logged in, or the login is not a valid address |

A few stale or low-difficulty rejections after new work or a new target are normal.

## Notes for miner authors

- New work arrives whenever the tips change (parallel blocks are routine here) and at least
  every 10 seconds. Switch as soon as it arrives.
- Report every nonce that meets the share target, not only the best one per batch; otherwise
  the bridge underestimates your hashrate.
- `crates/imoney-gpu-miner` is a working client in about 150 lines (`mine_stratum`).
