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

### 3.1 Design Philosophy
Algorithms that rely solely on arithmetic compute (e.g. SHA-256, kHeavyHash) are quickly dominated by custom chips. Money Printer is designed so that the cost of a hash is dominated by random reads from a large block of memory, which commodity graphics cards already do well. This is a design goal. The algorithm below has not been independently reviewed, and its resistance to specialised hardware is unproven.

### 3.2 Cache and dataset
Each epoch has a seed derived from the network's genesis hash and the epoch number. An epoch lasts 120,960 blocks by DAA score (one week at the target rate).

1. **Cache.** A list of 64-byte items built from the seed: a Blake3 hash chain, followed by two passes in which each item is rewritten from its neighbour and an item selected by its own contents. The cache must be built in order. It is 16 MB on mainnet and 64 KB on testnet.
2. **Dataset.** Item *i* of the dataset is computed from the cache alone: it starts from cache item *i* mod the cache size, is combined with 16 cache items selected by the running value, and is hashed. The dataset is 4.3 GB on mainnet and 4 MB on testnet.

### 3.3 Hash
The header's pre-proof-of-work hash and the nonce are expanded with Blake3 into a 64-byte mix of eight 64-bit lanes. Then, 32 times, a dataset position is taken from the mix, that item is read, and each lane is combined with it by XOR, rotation and multiplication, with a diffusion step across lanes. Every read position depends on all earlier reads. The final hash is Blake3 over the initial expansion and the final mix, and must not exceed the block's target.

### 3.4 Verification without the dataset
Because any dataset item can be computed from the cache, a node verifies a block by computing only the 32 items that block's hash reads. A verifying node therefore holds the cache, not the dataset. Miners hold the full dataset so that each read is a single memory access.

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
The selected chain ends in the tip with the most blue work. Each selected-chain block's exact ledger change is stored, so when a heavier chain appears the node undoes the old chain's changes back to the common block and applies the new chain's.

---

## 7. Peer-to-Peer Protocol

Nodes talk over TCP. Every message travels in a frame of a 4-byte network magic, a 4-byte big-endian length (at most 4 MiB) and a payload: a one-byte message tag followed by the canonical binary encoding of the message.

* **Handshake.** Each side opens with `Version` (protocol version, network, genesis hash, a random node ID, listening port). A peer on another protocol version, network or genesis is dropped. The node ID lets a node detect a connection to itself and collapse duplicate links between the same two nodes.
* **Relay.** Blocks and transactions are announced by hash (`InvBlock`, `InvTx`) and sent only to peers that ask for them.
* **Sync.** A node sends a *locator*: selected-chain hashes from its tip back to genesis, dense near the tip and exponentially sparser further back. The peer finds the newest block they share and returns blocks above it in `(level, hash)` order, in which parents always precede children, in batches of at most 200 blocks or 2 MB with a cursor for the next batch. A block that arrives before its parents waits in an orphan pool while the parents are requested.
* **Discovery.** Peers exchange the addresses of nodes that accept connections (`GetAddr`, `Addr`). A node keeps up to 8 outbound connections and accepts up to 64 inbound.
* **Misbehaviour.** Bytes that are not this protocol, and repeated invalid blocks, raise a peer's score; at the threshold the peer is disconnected and its IP refused for 10 minutes.
