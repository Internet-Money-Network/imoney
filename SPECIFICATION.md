# Internet Money (`IMN`) Protocol Specification

**Document Version:** 1.0.0  
**Status:** Working Draft  

---

## 1. Abstract

Internet Money (`IMN`) is an open-source decentralized cryptocurrency based on a directed acyclic graph of blocks (BlockDAG) ordered by the GHOSTDAG consensus protocol. It is engineered to provide sub-second transaction visibility with ~5-second block inclusion, with ASIC resistance as a design goal and decentralized mining distribution via the memory-hard **Money Printer** Proof-of-Work algorithm.

---

## 2. Consensus: GHOSTDAG at 5-Second Interval

### 2.1 Pacing and Block Window
Kaspa deployed GHOSTDAG initially at 1 BPS, subsequently targeting 10 BPS ($k=124$). In practice, high block frequencies impose severe node verification overhead and disadvantage miners operating on residential broadband due to propagation latency relative to block time.

Internet Money deliberately targets:
$$\Delta t = 5\,000\text{ ms} \quad (0.2\text{ blocks per second})$$

### 2.2 Convergence & $k$-Cluster Parameter
Under typical intercontinental peer latency ($\delta \approx 200\text{--}400\text{ ms}$), the ratio of propagation delay to block time is:
$$\frac{\delta}{\Delta t} \approx 0.04\text{--}0.08 \ll 1$$

With an honest network share $\alpha \ge 0.5$, a cluster parameter of $k = 8$ provides asymptotic security against double spending and guarantees that over $98\%$ of honestly produced blocks are accepted into the blue set.

---

## 3. Proof-of-Work: The "Money Printer" Engine

### 3.1 Algorithm
**Money Printer is FishHash with a network-specific seed.** FishHash is the Ethash-family, memory-hard algorithm designed for Iron Fish and also used by Karlsen. Internet Money did not design it and does not change it; `crates/imoney-pow/src/fishhash.rs` is a port of Iron Fish's reference implementation and is tested byte-for-byte against it.

Memory-hard algorithms make each hash depend on random reads from a large block of memory, which commodity graphics cards do well. This limits the advantage of custom chips; it does not rule them out. No FishHash ASIC is publicly known at the time of writing.

### 3.2 Parameters
| | Public networks (FishHash specification) | Private test networks (`--devnet`) |
| :--- | :--- | :--- |
| Light cache | 1,179,641 items of 64 bytes (about 75 MB) | 16,411 items (about 1 MB) |
| Dataset | 37,748,717 items of 128 bytes (about 4.6 GB) | 262,147 items (about 32 MB) |
| Dataset reads per hash | 32 rounds of 3 items | same |

The public test network uses the specification's sizes, so it is mined with graphics cards exactly as a main network would be. Private test networks use small sizes so that a CPU can mine them; a node reports its network's sizes in `GET /api/v1/info`.

The easiest target the public test network allows is 2^236, about a million hashes a block: a single CPU keeps the chain moving when nobody else is mining, and difficulty rises from there.

### 3.3 Network seed
FishHash builds its cache from a 32-byte seed. Each Internet Money network uses its own:

`seed = Blake3-derive-key("IMN 2026 Money Printer seed", genesis block hash)`

A different seed gives a different dataset, so the dataset of another FishHash network (or of another Internet Money network) is of no use here. The seed never changes, so there are no epochs and the dataset is built once.

### 3.4 Block hashing
The input to FishHash is 40 bytes: the header's 32-byte pre-proof-of-work hash followed by the 8-byte little-endian nonce. The 32-byte output, read as a big-endian number, must not exceed the block's target.

### 3.5 Verification without the dataset
Any dataset item can be computed from the light cache alone, so a node verifies a block by computing only the 96 items that block's hash reads. A verifying node holds the cache, not the dataset. Miners hold the full dataset so that each read is a single memory access.

---

## 4. Monetary Policy & Emission Curve

### 4.1 Smooth 4-Year Halving (No Opening Premine)
* **Genesis Block:** Zero premine.
* **Launch Block Subsidy:** $5.00000000\text{ IMN}$ per block ($500\text{M}$ atoms).
* **Era Duration:** $25,228,800$ blocks (4 × 365 days).

### 4.2 Mathematical Model
For a block with cumulative DAA score $s$, the era index $E$ is:
$$E = \left\lfloor \frac{s}{25\,228\,800} \right\rfloor$$

The block subsidy $R(s)$ in atomic units is defined piecewise:
$$R(s) = \begin{cases} 
500\,000\,000 / 2^E & \text{for } E \in \{0, 1, 2, 3\} \\
31\,250\,000 & \text{for } E \ge 4 \text{ (Permanent Floor)}
\end{cases}$$

### 4.3 Supply Metrics Table
| Era | Years | Blocks | Reward / Block | Total Mined in Era | Cumulative Supply |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **0** | 0 – 4 | 0 – 25,228,799 | 5.00000000 IMN | 126,144,000 IMN | 126,144,000 IMN |
| **1** | 4 – 8 | 25,228,800 – 50,457,599 | 2.50000000 IMN | 63,072,000 IMN | 189,216,000 IMN |
| **2** | 8 – 12 | 50,457,600 – 75,686,399 | 1.25000000 IMN | 31,536,000 IMN | 220,752,000 IMN |
| **3** | 12 – 16 | 75,686,400 – 100,915,199 | 0.62500000 IMN | 15,768,000 IMN | 236,520,000 IMN |
| **4+** | 16+ | 100,915,200+ | **0.31250000 IMN** | ~1,971,000 IMN / yr | Uncapped (<1% tail) |

---

## 5. Difficulty Adjustment Algorithm (DAA)

Difficulty is recomputed for every block from a rolling window of the 144 most recent blocks in its past ($720	ext{ seconds} pprox 12	ext{ minutes}$). Genesis is excluded from the window.

The network hashrate is estimated as the work done across the window divided by the time the window spans, where the work of a block with target $T$ is $W = 2^{256} / (T + 1)$:
$$H = \frac{\sum W_i - W_{oldest}}{t_{newest} - t_{oldest}}$$

The next target is the one this hashrate meets once per block interval:
$$T_{next} = \frac{2^{256}}{H \cdot \Delta t}$$

Two limits apply. $T_{next}$ never exceeds the network's maximum target, and it may differ from the selected parent's target by at most a factor of 2 in either direction, so a single unusually fast or slow block cannot cause a large swing. With fewer than 8 blocks in the window the maximum target is used. All arithmetic is integer arithmetic.

A node recomputes the expected value for every block it receives; a header carrying any other `bits` value is invalid.

---

## 6. Block Validity and Transaction Acceptance

### 6.1 Header rules
A node recomputes `blue_score`, `blue_work`, `daa_score` and `bits` from the block's parents and rejects the block if any differ. A block's timestamp must be later than the median timestamp of the 41 most recent blocks in its past and at most 2 minutes ahead of the local clock. No parent may be an ancestor of another parent, and a block may merge at most $10k = 80$ blocks.

### 6.2 Acceptance order
A block's own transactions do not change the ledger when the block is mined. They are *accepted* by the next selected-chain block that merges it. That block applies the transactions of every block in its mergeset in a fixed order: the selected parent first, then the remaining blocks by ascending blue work, ties broken by hash. A transaction that cannot be spent at its turn (its input was already spent earlier in the order, its signature is invalid, or it spends an immature reward) is skipped; it does not invalidate the block that carries it.

Because the order depends only on the DAG, every node with the same blocks computes the same ledger regardless of the order in which the blocks arrived.

### 6.3 Rewards and fees
The coinbase transaction names the miner's payout script in at most one output worth at most the block subsidy. When a block is merged as blue, the ledger creates one reward output for its miner worth that amount plus the miner's share of the fees of the block's accepted transactions. Red blocks earn no reward, and the miner's share of fees from transactions accepted out of red blocks is burned.

A transaction may name a *service script*: the node that served the payment, chosen by the wallet and covered by the signature. When such a transaction is accepted, half of its fee (rounded down) is paid to that script as an extra output at index `outputs.len()`, spendable immediately, and the miner's share is the remainder. A transaction without a service script pays its whole fee to the miner. Because anyone may name their own address, this works as a fee discount for those who run a node rather than as an income guaranteed by the protocol. A reward may be spent once it is buried by the coinbase maturity depth (20 blue-score on testnet).

### 6.4 Chain reorganisation
The selected chain ends in the eligible tip with the most blue work. Each selected-chain block's exact ledger change is stored, so when a heavier chain appears the node undoes the old chain's changes back to the common block and applies the new chain's.

### 6.5 Finality
The *finality point* is the selected-chain block 8,640 blue score (12 hours) below the current tip. A tip is eligible only if its selected chain passes through the finality point, so a chain that forks off deeper than that is never adopted, whatever its work. This stops rented hashpower from rewriting old history. The cost is that two parts of the network separated for longer than the finality depth will not rejoin by themselves. A node that sees a heavier chain it must refuse reports `finality_conflict` in its status. A node also records the depth and time of any reorganisation of 3 or more blocks, so that software accepting payments can pause while one is recent.

---

## 7. Peer-to-Peer Protocol

Nodes talk over TCP. Every message travels in a frame of a 4-byte network magic, a 4-byte big-endian length (at most 4 MiB) and a payload: a one-byte message tag followed by the canonical binary encoding of the message.

* **Handshake.** Each side opens with `Version` (protocol version, network, genesis hash, a random node ID, listening port). A peer on another protocol version, network or genesis is dropped. The node ID lets a node detect a connection to itself and collapse duplicate links between the same two nodes.
* **Relay.** Blocks and transactions are announced by hash (`InvBlock`, `InvTx`) and sent only to peers that ask for them.
* **Sync.** A node sends a *locator*: selected-chain hashes from its tip back to genesis, dense near the tip and exponentially sparser further back. The peer finds the newest block they share and returns blocks above it in `(level, hash)` order, in which parents always precede children, in batches of at most 200 blocks or 2 MB with a cursor for the next batch. A block that arrives before its parents waits in an orphan pool while the parents are requested.
* **Discovery.** Peers exchange the addresses of nodes that accept connections (`GetAddr`, `Addr`). A node keeps up to 8 outbound connections and accepts up to 64 inbound.
* **Misbehaviour.** Bytes that are not this protocol, and repeated invalid blocks, raise a peer's score; at the threshold the peer is disconnected and its IP refused for 10 minutes.

---

## 8. Invoices and Payment Levels

### 8.1 Invoice ID
A payment may name the invoice it settles. The transaction's payload is then the ASCII bytes `imn-invoice:` followed by the invoice ID: 1 to 64 characters from `A-Z a-z 0-9 - _ .`. The payload is covered by the signature, so the payer commits to that invoice and nobody else can relabel the payment.

A payment request is the receiving address with the amount and invoice ID attached: `imn:q...?amount=12.5&invoice=INV-1042`.

### 8.2 Tracking
A node indexes accepted transactions by invoice ID and removes the entry if a reorganisation un-accepts the transaction. `GET /api/v1/invoice/:id?address=:addr` returns every payment naming that invoice and paying that address, pending or accepted, with three totals:

| Level | Meaning |
| :--- | :--- |
| `seen` | The node has the payment (in its mempool or accepted). It may still be replaced. |
| `included` | The payment has been accepted into the ledger (1 or more confirmations). |
| `final` | The payment has at least the node's `final_confirmations` (default 60, about 5 minutes) and the node is not reporting `network_alert`. |

`network_alert` is set while the node is refusing a heavier chain for finality, or for 30 minutes after a reorganisation of 3 or more blocks. Nothing is reported as final while it is set.

The number of confirmations to require is a judgement about value at risk: reversing a payment costs an attacker roughly the hashpower to out-mine the network for that many blocks. The default is a starting point, not a guarantee.

### 8.3 Multi-signature addresses
An address of type `ScriptHash` (type byte 2) locks coins to the hash of a script: one byte for the threshold `m`, one for the key count `n` (1 ≤ m ≤ n ≤ 16), then `n` Ed25519 public keys in strictly ascending byte order. The hash is `Blake3-derive-key("IMN 2026 multisig script", script)`. Requiring sorted keys gives each set of keys and threshold exactly one address.

An input spending such a coin carries the script followed by exactly `m` entries of one byte (the signing key's position in the script) and a 64-byte signature, with positions strictly ascending. Each signature is over the ordinary signing hash of that input. The signing hash does not cover signature scripts, so key holders sign independently and in any order.

### 8.4 Keys
Nodes do not create, store or receive private keys. Wallets sign locally; the reference wallet uses a WebAssembly build of the same transaction code the node runs. A wallet's key is derived from a 12-word BIP-39 recovery phrase as `Blake3-derive-key("IMN 2026 wallet key 0", BIP-39 seed)`.
