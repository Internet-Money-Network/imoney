# 8. Mining

Mining is a loop: ask a node for a block to work on, search for a number (the **nonce**) that
makes the block's proof-of-work hash small enough, and hand the block back. Whoever finds it
is paid the block reward.

## The proof of work

The algorithm is **Hallmark**: FishHash with a seed specific to this network. Each hash reads
from random places in a 4.6 GB table, which suits the memory on ordinary graphics cards. A
node checks a block using a 75 MB table, so verifying is cheap even though mining is not.

Private networks (`--devnet`) use a 32 MB table so a CPU can mine them. A node reports which
sizes its network uses in `GET /api/v1/info` (`pow_light_cache_items`, `pow_dataset_items`).

## Mining with the reference miners

On a private network, the CPU miner is enough:

```bash
cargo run --release -p imoney-miner -- --node http://127.0.0.1:28556 --address imntest:q…
```

On the public test network, use the GPU miner (OpenCL; any recent AMD or NVIDIA card with
6 GB or more):

```bash
cargo run --release -p imoney-gpu-miner -- --list-devices
cargo run --release -p imoney-gpu-miner -- --node http://127.0.0.1:18556 --address imntest:q…
```

The first start builds the 4.6 GB table, which takes several minutes, and saves it beside the
program so later starts are quick. Before mining, it checks the card's hashes against the
CPU's and refuses to mine if they differ.

## Through a pool

Solo mining pays 5 IMN when you find a block and nothing otherwise. A pool shares rewards by
work done, so income is steadier. Point the GPU miner at a pool instead of a node:

```bash
cargo run --release -p imoney-gpu-miner -- --stratum pool.internetmoneynetwork.org:18557 --address imntest:q…
```

Your address is your login. There is nothing to register.

## Writing your own miner

Two calls:

```
GET /api/v1/mining/template?address=<payout address>
→ { "block": { "header": { …, "nonce": 0 }, "transactions": [ … ] },
    "pre_pow_hash": [ …32 bytes… ], "target_hex": "…" }
```

```
POST /api/v1/mining/submit
{ "block": { …the same block, with header.nonce set… } }
→ { "success": true, "block_hash": "…" }
```

The search itself:

1. The input to the hash is 40 bytes: `pre_pow_hash`, then the nonce as 8 little-endian bytes.
2. Compute FishHash of it with this network's table.
3. Read the 32-byte result as a big-endian number. If it is at most the target, you have a
   block: put the nonce in the header and submit.

Change nothing else in the block. The node compares everything but the nonce with what it
would have built.

**Fetch a new template every second or two.** Blocks arrive from others all the time, and a
template is out of date as soon as the tips change. A stale block is refused with a reason,
which is routine and not an error in your miner.

If the node was started with `--rpc-token`, send `Authorization: Bearer <token>` with the
submit call.

## Speaking Stratum

Existing mining software speaks Stratum, not HTTP. `imoney-stratum` is a bridge between the
two, and with `--pool` it is a complete small pool with share accounting, a fee, automatic
payouts and a status page. The messages are written up in [STRATUM.md](../STRATUM.md).

## What a miner earns

- The block reward starts at 5 IMN and halves every four years, down to a permanent
  0.3125 IMN.
- Fees of the payments the block carries are added. When a payment names a service address,
  that address gets half of the fee and the miner the other half.
- A reward can be spent after 20 confirmations.
- When several blocks are found side by side, nearly all of them are kept and paid. A block
  that arrives very late compared with the others can be marked *red*: it is still part of
  the chain, but earns nothing. Expect a small fraction of blocks to end up that way.

## Things that catch people out

- **A table from another network.** The seed comes from the network's first block, so a
  table built for one network gives wrong hashes on another. The miners name their saved
  table after the network to avoid this.
- **A stale binary.** After the network's rules or seed change, rebuild every program, not
  only the node. A miner built for the old rules mines blocks that are all refused.
- **Never mine on rented cloud credits.** Most providers' free tiers forbid it.

Next: [reference](09-reference.md).
