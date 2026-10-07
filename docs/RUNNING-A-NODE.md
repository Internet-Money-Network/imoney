# Running an Internet Money node

This covers the test network. There is no main network yet.

## What a node needs

| | Verifying node | Mining node |
| :--- | :--- | :--- |
| CPU | 2 cores | as many as you want to mine with |
| Memory | 2 GB to start; see [Growth](#growth) | the same, plus the mining dataset (32 MB on testnet, 4.6 GB at mainnet sizes) |
| Disk | 40 GB SSD | the same |
| Network | one open TCP port (18555) | the same |

## Build and run

```bash
cargo build --release

# A node that follows the network
./target/release/imoney-node --peers seed.internetmoneynetwork.org:18555

# The wallet and dashboard are then at http://127.0.0.1:18556
```

To mine, either let the node mine by itself or point the miner at it:

```bash
./target/release/imoney-node --auto-mine --mining-address imntest:q...
# or
./target/release/imoney-miner --node http://127.0.0.1:18556 --address imntest:q...
```

Create the address in the wallet at `/wallet` first, and write down its recovery phrase. Without
`--mining-address` the node creates a key of its own in `<data-dir>/miner-key.hex`.

## Options

| Option | Default | What it does |
| :--- | :--- | :--- |
| `--data-dir` | `./data` | Where the database, peer list and miner key are kept. |
| `--p2p-bind` | `0.0.0.0:18555` | Address other nodes connect to. Open this port in your firewall. |
| `--rpc-bind` | `127.0.0.1:18556` | Address of the HTTP API, wallet and dashboard. Keep it private unless you mean to run a public API. |
| `--peers` | none | Nodes to connect to, as `host:port`, comma-separated. A host name may resolve to several nodes. |
| `--auto-mine` | off | Mine with this machine's CPU. |
| `--mining-address` | node's own key | Where mining rewards go. |
| `--service-address` | the mining address | Where this node's half of the fee goes on payments that name it. |
| `--prune` | off | Delete the contents of blocks older than 36 hours. See below. |
| `--final-confirmations` | 60 | Confirmations before this node reports a payment as final. |
| `--rpc-token` | none | Require this token to submit blocks over the API. |

## Full history or pruned

A node keeps every block by default. With `--prune` it deletes the transactions of blocks buried
more than 36 hours deep and keeps only block headers and the current ledger. It still checks
every new block and payment in full.

- **Seed nodes must not prune.** Only a node with the full history can bring a new node up to date.
- **A shop's node can prune.** It gives up two things: it cannot look up payments older than
  36 hours by transaction or invoice number, and it cannot serve history to other nodes.
- The database file does not shrink when pruning starts; the freed space is reused, so it stops growing.

## Running a seed node

A seed is an ordinary node with the full history, a fixed address and good uptime. Other nodes use
it to find the network.

1. Start the node as a service: see [`deploy/imoney-node.service`](../deploy/imoney-node.service)
   for systemd or [`deploy/docker-compose.seed.yml`](../deploy/docker-compose.seed.yml) for Docker.
   Do not pass `--prune`.
2. Open TCP port 18555 to the internet. Leave 18556 closed.
3. List the other seeds in `--peers` so the seeds stay connected to each other.
4. Add the machine's IP address to the seed DNS name as an `A` record. Several `A` records on one
   name are fine; nodes try all of them. On Cloudflare the record must be **DNS only** (grey
   cloud): the proxy passes web traffic only, and nodes talk on their own port.

Three or four seeds with different hosting companies and in different regions make it unlikely
that the network splits or that a new node cannot find it.

## Running a node for a shop

- Keep the API private: the store's server should reach it over localhost or a private network.
- `--prune` is fine.
- Set `--service-address` to an address you control. Wallets that send through your node then
  return half of each fee to you.
- Choose `--final-confirmations` for what you sell. The default of 60 (about five minutes) is a
  starting point. Reversing a payment costs an attacker the hashpower to out-mine the network for
  that many blocks, so higher-value goods deserve a longer wait while the network is small.
- Watch `network_alert` in `/api/v1/info`. While it is set, the node reports nothing as final.

## Health checks

`GET /api/v1/info` reports, among other things:

| Field | Healthy value |
| :--- | :--- |
| `virtual_blue_score` | rising by about 12 a minute |
| `finality_conflict` | `false`. `true` means a heavier chain exists that this node refuses; the network may have split and needs a human to look. |
| `network_alert` | `false` |
| `mempool_size` | not growing without bound |

`GET /api/v1/peers` should list at least one peer.

## Growth

Measured over 6,000 empty blocks, a node's database grows by about 1.5 to 2.3 KB per block,
which is 26 to 39 MB a day or roughly 10 to 14 GB a year. With `--prune` the same run grew by
about 1.25 KB per block (22 MB a day): on a chain with few payments most of what a block costs is
its header, its consensus record and its miner's reward coin, none of which pruning removes.
Pruning saves the most when blocks carry many payments.

Block size is capped at 100 KB, so the most the chain can grow is about 1.7 GB a day, and only if
every block is full. You can repeat the measurement with
`cargo test --release -p imoney-node -- --ignored disk_use --nocapture`.

One limit is not solved yet: a node keeps every block header in memory, pruned or not. That
grows by roughly 2 GB a year, so plan to move to a machine with more memory within the first year.

## Upgrading during testnet

The test network is reset when the block format or consensus rules change. After such an
upgrade, stop the node and delete the data directory except `miner-key.hex`, then start it again.
