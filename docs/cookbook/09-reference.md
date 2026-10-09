# 9. Reference

Things to look up. The exact rules are in [SPECIFICATION.md](../../SPECIFICATION.md); this
page is the short version a developer needs day to day.

## Numbers

| | |
| :--- | :--- |
| Unit | 1 IMN = 100,000,000 atoms |
| Block time | about 5 seconds |
| Block reward | 5 IMN, halving every 4 years, never below 0.3125 IMN |
| Reward can be spent after | 20 confirmations |
| Reported as final after | 60 confirmations (about 5 minutes); set with `--final-confirmations` |
| Never reorganised beyond | 8,640 blocks deep (12 hours) |
| Minimum fee | 10 atoms per byte |
| An ordinary payment | about 240 bytes, so at least 2,400 atoms in fee |
| Largest transaction | 50,000 bytes |
| Largest block | 100,000 bytes |
| Parents per block | up to 16 |
| Block timestamps | at most 30 seconds ahead of a node's clock |
| Invoice ID | 1 to 64 characters from `A-Z a-z 0-9 - _ .` |
| Multi-signature | any m of up to 16 keys |
| Coins merged at once | up to 300 |

## Addresses

| Network | Prefix | Example |
| :--- | :--- | :--- |
| Test networks, public and private | `imntest:` | `imntest:qqzh3r96a2v3454gxue2a59l85nvrup6mllm6xmr84jenq7e2fd6ccalehw` |
| Main network | `imn:` | none yet |

Ordinary and multi-signature addresses look alike and are paid the same way. To tell them
apart, decode the address and read its type. Addresses carry a checksum, so a typo is refused.

Payment request: `<address>?amount=<IMN>&invoice=<ID>`. Both parts after `?` are optional.

## Ports

| Port | What | Open to |
| :--- | :--- | :--- |
| 18555 | Node to node | Everyone |
| 18556 | HTTP API, explorer and wallet pages | Your own machines, unless you mean to run a public API |
| 18557 | Stratum (pool or bridge) | Miners |
| 18558 | Pool status page | Everyone, if you run a pool |

## HTTP API

All under `/api/v1/`. All `GET` unless marked.

| Path | Returns |
| :--- | :--- |
| `info` | Network, tip, difficulty, settings of this node |
| `stats` | Hashrate, block time, supply, latest payments |
| `tips` | Hashes of the blocks nothing has built on yet |
| `peers` | Nodes this one is connected to |
| `blocks?limit=N` | Newest blocks, up to 500 |
| `block/{hash}` | One block with its payments and miner |
| `tx/{id}` | A payment: status, confirmations, inputs, outputs, invoice |
| `address/{address}/balance` | What the address holds |
| `address/{address}/utxos` | Its coins |
| `address/{address}/history?limit=N&before=…` | What it received and sent, newest first |
| `invoice/{id}?address={address}` | What has been paid towards an invoice, by level |
| `ws/address/{address}` (WebSocket) | A message when a payment arrives or the balance changes |
| `tx/broadcast` (`POST`) | Accepts a signed transaction |
| `mining/template?address={address}` | A block to mine |
| `mining/submit` (`POST`) | Accepts a mined block |

Worked examples of each are in [chapter 3](03-reading-the-chain.md),
[chapter 4](04-keys-and-sending.md) and [chapter 8](08-mining.md).

## Messages a node gives when it refuses

| Message | Meaning | What to do |
| :--- | :--- | :--- |
| `Fee of N atoms is below the minimum of M atoms for a B-byte transaction` | Too little fee for its size | Pay at least M |
| `Input already spent by pending transaction …` | Another waiting payment spends the same coin | Wait for a block, or build from other coins |
| `Mempool is full and this transaction pays less per byte than everything in it` | The node is saturated | Raise the fee or try later |
| `Public key does not match address hash` | Signed with a key that does not own the coin | Use the key of the address the coin is locked to |
| `Public key verification failed: …` | The signature is wrong, or made for another network | Sign again, checking the network |
| `Invalid multi-signature spend: …` | Wrong number or order of signatures, or a key not in the script | See [chapter 7](07-shared-wallets.md) |
| `No inputs provided` / `No outputs provided` | An empty transaction | Check the coin list was not empty |
| `Block timestamp is too far in the future` | A miner's clock is ahead | Set the clock |
| `Parent … is more than the finality depth behind the block's other parents` | A block built on something very old | Fetch a fresh template |
| `The database holds a different network's chain…` | The data directory belongs to another network | Use another `--data-dir` |

## Tidying coins

An address that has received many payments holds many coins, and spending hundreds at once
makes a large, costly transaction. Merge them when things are quiet:

```rust
let tx = Transaction::build_consolidation(&key, Network::Testnet, fee_atoms, coins, None)?;
```

It spends up to 300 coins and creates one, back at the same address. The reference wallet
offers the same thing as a button.

## Words

| Word | Meaning |
| :--- | :--- |
| **Atom** | The smallest unit: a hundred-millionth of an IMN |
| **Blue / red** | GHOSTDAG's two labels for blocks. Blue blocks are well connected to the rest and earn rewards. A red block arrived too far out of step; it is kept but earns nothing |
| **Blue score** | How many blue blocks are behind a block. Confirmations are counted in it |
| **Change** | The part of a spent coin that comes back to the sender as a new coin |
| **Coin** | An amount locked to an address. Also "output" or "UTXO" |
| **Coinbase** | The first transaction in a block, which creates the miner's reward |
| **Confirmation** | One blue block added on top of the block that accepted a payment |
| **DAG** | The shape the blocks make when a block can have several parents |
| **Devnet** | A private network started with `--devnet` |
| **Final** | Buried deeply enough that this node will not accept its reversal |
| **GHOSTDAG** | The rule that puts a DAG of blocks into one agreed order |
| **Hallmark** | This network's proof-of-work algorithm (FishHash with its own seed) |
| **Invoice ID** | A reference signed into a payment so the recipient can match it to an order |
| **Mempool** | Valid payments a node holds that are not yet in a block |
| **Mergeset** | The blocks a block brings into the order beyond its main parent |
| **Nonce** | The number a miner varies to find a valid proof of work |
| **Pruning** | Deleting the contents of old blocks while keeping balances |
| **Reorganisation** | The agreed order of recent blocks changing because heavier work arrived |
| **Selected parent** | The parent a block's place in the order is counted from |
| **Service address** | An address a payment may name to receive half of its fee, for the node that served it |
| **Tip** | A block nothing has built on yet |
| **UTXO** | Unspent transaction output: a coin |
