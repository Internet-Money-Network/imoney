# Load testing a node

`imoney-loadtest` floods one test node with real, signed payments at a chosen rate while many
clients watch addresses over WebSocket, as checkout pages do. It reports how long payments took
to submit and to reach a block, how fast the API answered, block intervals and mempool size.

Run it against a throwaway node, never one holding a chain you care about: the node has to mine
to the tool's own funding address.

```bash
cargo build --release

# 1. The address the node must mine to
./target/release/imoney-loadtest --print-address

# 2. A throwaway node mining to it
./target/release/imoney-node --devnet loadtest --data-dir ./loadtest-data --rpc-bind 127.0.0.1:18796 \
    --p2p-bind 127.0.0.1:18795 --auto-mine --mining-address <address from step 1>

# 3. The test: 70 payments a second for a minute, with 1,000 watchers
./target/release/imoney-loadtest --node http://127.0.0.1:18796 --tps 70 --duration 60 \
    --wallets 3000 --watchers 1000 --workers 32
```

Each test wallet can have one unconfirmed payment at a time, so use at least 20 to 30 wallets
per payment-per-second. "Wallet not ready" in the result means the tool ran out of wallets, not
that the node refused anything.

## Reference results

One machine (24 threads) running the node, its miner and the load generator together, on
2026-10-08. Blocks are limited to 100 KB, which is about 80 payments a second.

| Asked for | Accepted | Submit time (median / worst) | API response (median / worst) | Time to a block (median / 95%) | Largest mempool |
| :--- | :--- | :--- | :--- | :--- | :--- |
| 20 /s, 200 watchers | 20.0 /s | 0.7 ms / 9.8 ms | 0.3 ms / 2.1 ms | 2.8 s / 7.6 s | 136 |
| 70 /s, 1,000 watchers | 70.0 /s | 0.9 ms / 46 ms | 0.4 ms / 2.5 ms | 4.7 s / 19 s | 908 |
| 200 /s, 1,000 watchers | 139 /s (tool ran out of wallets) | 0.9 ms / 69 ms | 0.3 ms / 55 ms | 46 s / 108 s | 5,135 |

Above capacity the node stayed responsive and rejected nothing; payments queued, took longer
to reach a block, and the queue emptied a minute after the flood stopped.

What these runs do not cover: more than one node (no network propagation), a large existing
chain or UTXO set, a full 50 MB mempool, or runs longer than a few minutes.
