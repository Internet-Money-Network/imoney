# Internet Money (`IMN`) Protocol Specification

**Document Version:** 1.0.0  
**Status:** Working Draft  

---

## 1. Abstract

Internet Money (`IMN`) is an open-source proof-of-work cryptocurrency based on a directed acyclic graph of blocks (BlockDAG) ordered by the GHOSTDAG consensus protocol, with a block every 5 seconds. Its main addresses are quantum-safe: coins are locked to an Ed25519 key and a SQIsign key, and a spend needs both signatures (§8.11). Tokens, offers, escrow, subscriptions, vaults and hash locks are consensus rules rather than contracts (§6.4, §8). Mining uses the memory-hard **Hallmark** algorithm, suited to commodity graphics cards. The [whitepaper](https://internetmoneynetwork.org/whitepaper) explains the design; this document specifies it.

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

## 3. Proof-of-Work: The Hallmark Engine

### 3.1 Algorithm
**Hallmark is FishHash with a network-specific seed.** FishHash is the Ethash-family, memory-hard algorithm designed for Iron Fish and also used by Karlsen. Internet Money did not design it and does not change it; `crates/imoney-pow/src/fishhash.rs` is a port of Iron Fish's reference implementation and is tested byte-for-byte against it.

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

`seed = Blake3-derive-key("IMN 2026 Hallmark seed", genesis block hash)`

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

### 4.2 Maintenance Fund
For the first 18,921,600 blocks (DAA scores below that: three years of 5-second blocks), 5% of each blue block's subsidy, rounded down to the atom, is paid by the ledger itself to the network's maintenance address, fixed in the consensus rules: a second reward output beside the miner's, at index 1 of the block's reward outpoint, maturing like any block reward. During that period a block's coinbase may claim at most the subsidy less that share; transaction fees remain wholly the miner's. After it, the miner may claim the whole subsidy. The share comes out of the subsidy, so the emission schedule and total supply are unchanged: about 4.73 million IMN over the period, 2% of the supply at year 16. Genesis pays neither a reward nor a share.

### 4.3 Mathematical Model
For a block with cumulative DAA score $s$, the era index $E$ is:
$$E = \left\lfloor \frac{s}{25\,228\,800} \right\rfloor$$

The block subsidy $R(s)$ in atomic units is defined piecewise:
$$R(s) = \begin{cases} 
500\,000\,000 / 2^E & \text{for } E \in \{0, 1, 2, 3\} \\
31\,250\,000 & \text{for } E \ge 4 \text{ (Permanent Floor)}
\end{cases}$$

### 4.4 Supply Metrics Table
| Era | Years | Blocks | Reward / Block | Total Mined in Era | Cumulative Supply |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **0** | 0 – 4 | 0 – 25,228,799 | 5.00000000 IMN | 126,144,000 IMN | 126,144,000 IMN |
| **1** | 4 – 8 | 25,228,800 – 50,457,599 | 2.50000000 IMN | 63,072,000 IMN | 189,216,000 IMN |
| **2** | 8 – 12 | 50,457,600 – 75,686,399 | 1.25000000 IMN | 31,536,000 IMN | 220,752,000 IMN |
| **3** | 12 – 16 | 75,686,400 – 100,915,199 | 0.62500000 IMN | 15,768,000 IMN | 236,520,000 IMN |
| **4+** | 16+ | 100,915,200+ | **0.31250000 IMN** | ~1,971,000 IMN / yr | Uncapped (<1% tail) |

---

## 5. Difficulty Adjustment Algorithm (DAA)

Difficulty is recomputed for every block from the last step of its selected chain. Let $P$ be the block's selected parent and $G$ the selected parent of $P$. The step from $G$ to $P$ added $n$ blocks to the DAG: $P$ itself and every block it merged. On schedule that takes $n$ block intervals. The target moves from the parent's by how far the step was off:

$$T_{next} = T_P \cdot 2^{\,(t_P - t_G - n \cdot 5	ext{ s}) / 100	ext{ s}}$$

The exponent is limited to $[-1, 1]$, so one step at most doubles or halves the target, and $T_{next}$ never exceeds the network's maximum target. The first block after genesis uses the maximum target. The power of two is computed in 16.16 fixed point with a cubic approximation, in integers only, so every node gets the same value.

The 100-second half life (20 blocks) was chosen by simulation. With it the difficulty reaches within a factor of two of a fifty-fold jump in hashrate in about 120 blocks (two minutes), recovers from a fifty-fold drop in about 25 blocks (ten minutes), and moves by under a fifth from ordinary luck in block times. A windowed average over 144 blocks, which this replaced, took about 85 minutes to recover from the same drop.

A block's timestamp may be at most 30 seconds ahead of the receiving node's clock. With a wider allowance, a miner stamping its blocks far ahead could make blocks come a few percent too fast and widen the swings in difficulty; simulated at 30 seconds, a miner with 30% of the hashrate doing so changes neither the block time nor its own share of blocks. A miner that switches a large hashrate on only while difficulty is low earns within 2% of what steady miners earn per hash.

Looking only at the last step has two further effects. A long silence, such as all miners stopping for a day, eases the difficulty by one factor of two when mining resumes; it does not leave a backlog of easy blocks to be mined in a burst. And a single false timestamp changes the next block's difficulty by at most a factor of two, which the following honest timestamp reverses.

A node recomputes the expected value for every block it receives; a header carrying any other `bits` value is invalid.

---

## 6. Block Validity and Transaction Acceptance

### 6.1 Header rules
A node recomputes `blue_score`, `blue_work`, `daa_score` and `bits` from the block's parents and rejects the block if any differ. A block's timestamp must be later than the median timestamp of the 41 most recent blocks in its past and at most 30 seconds ahead of the local clock. No parent may be an ancestor of another parent, and a block may merge at most $10k = 80$ blocks.

A block may not name a parent whose blue score is more than the finality depth (8,640) below that of its best parent. Such a block could no longer change the ledger, and the limit bounds the work of checking any block's parents.

### 6.2 Acceptance order
A block's own transactions do not change the ledger when the block is mined. They are *accepted* by the next selected-chain block that merges it. That block applies the transactions of every block in its mergeset in a fixed order: the selected parent first, then the remaining blocks by ascending blue work, ties broken by hash. A transaction that cannot be spent at its turn (its input was already spent earlier in the order, its signature is invalid, or it spends an immature reward) is skipped; it does not invalidate the block that carries it.

Because the order depends only on the DAG, every node with the same blocks computes the same ledger regardless of the order in which the blocks arrived.

### 6.3 Rewards and fees
The coinbase transaction names the miner's payout script in at most one output worth at most the block subsidy. When a block is merged as blue, the ledger creates one reward output for its miner worth that amount plus the miner's share of the fees of the block's accepted transactions. Red blocks earn no reward, and the miner's share of fees from transactions accepted out of red blocks is burned.

A transaction may name a *service script*: the node that served the payment, chosen by the wallet and covered by the signature. When such a transaction is accepted, half of its fee (rounded down) is paid to that script as an extra output at index `outputs.len()`, spendable immediately, and the miner's share is the remainder. A transaction without a service script pays its whole fee to the miner. Because anyone may name their own address, this works as a fee discount for those who run a node rather than as an income guaranteed by the protocol. A reward may be spent once it is buried by the coinbase maturity depth (20 blue-score on testnet).

### 6.4 Native tokens
Any output may carry, besides its IMN, an amount of one token or that token's *mint authority*. Fees are always paid in IMN. On the wire an output ends with a flag byte: 0 for no token, or 1 followed by the 32-byte token ID, an 8-byte amount and a byte that is 1 for a mint authority and 0 otherwise. A transaction ends with a flag byte: 0, or 1 followed by the issued token's name, ticker (each length-prefixed), one byte of decimals and a length-prefixed URL.

- **Issue.** A transaction with an `issue` record creates a token whose ID is `Blake3-derive-key("IMN 2026 token id", transaction id ++ index)` of the outpoint its first input spends. It must create at least one output carrying that ID. Name: 1 to 32 bytes of UTF-8 without control characters. Ticker: 1 to 8 of `A-Z 0-9`. Decimals: 0 to 12. URL: up to 120 printable ASCII bytes. Names and tickers are labels and need not be unique.
- **Conservation.** For every token a transaction neither issues nor holds a mint authority for, the amounts it spends and creates are equal. A transaction that drops a token is refused, not treated as burning it. Tokens are destroyed by sending them to the burn script (version 0xff, empty), which no key can spend.
- **Minting.** A transaction that spends a mint authority for a token, or issues it, may create any amount of it and any number of new authorities for it. An authority output carries amount 0; any other token output carries a positive amount. A token whose authorities are all spent without being recreated can never be minted again.
- **Coinbase.** A coinbase may not issue or carry tokens.

Token outputs are locked like any other: an ordinary address, a multi-signature address or a hash lock (§8.4). A hash lock that holds tokens moves them with its single output, paying the fee from the IMN the lock holds.

### 6.5 Chain reorganisation
The selected chain ends in the eligible tip with the most blue work. Each selected-chain block's exact ledger change is stored, so when a heavier chain appears the node undoes the old chain's changes back to the common block and applies the new chain's.

### 6.6 Finality
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

### 8.4 Hash locks
An address of type `HashLock` (type byte 3) locks coins to the hash of a 104-byte script: the recipient's Ed25519 public key (32 bytes), the refunder's (32), a SHA-256 hash (32), and a blue score as an 8-byte big-endian number. The address hash is `Blake3-derive-key("IMN 2026 hash lock script", script)`.

An input spending such a coin carries the script, one branch byte, a 64-byte signature over the ordinary signing hash of that input, and for a claim a 32-byte secret:

- **Claim** (branch 0): signed by the recipient, and `SHA-256(secret)` equals the hash in the script. Valid only in blocks whose blue score is below `refund_after`, so a claim and a refund are never both possible.
- **Refund** (branch 1): signed by the refunder. Valid only when the blue score of the block that accepts the transaction is at least the script's.

Two parties who lock coins for each other under one hash, here and on another chain, can exchange them without a third party: claiming either side publishes the secret that claims the other, and an unclaimed lock returns to its owner after its deadline. The refund deadline on the chain where the secret is revealed second must be the later of the two, with enough margin for the other party to act.

### 8.5 Escrow
An address of type `Escrow` (type byte 4) locks coins to the hash of a 105-byte script: the buyer's, seller's and arbiter's Ed25519 public keys (32 bytes each, all different), a blue score as an 8-byte big-endian number (the deadline), and one byte that is 1 if the seller collects after the deadline and 0 if the buyer does. The address hash is `Blake3-derive-key("IMN 2026 escrow script", script)`.

An input spending such a coin carries the script, one role byte and a 64-byte signature over the ordinary signing hash of that input. Each role is signed by one key and may pay only certain parties, meaning outputs locked to that party's ordinary address:

| Role | Signed by | May pay |
| :--- | :--- | :--- |
| 0 buyer | buyer | seller |
| 1 seller | seller | buyer |
| 2 arbiter | arbiter | buyer, seller, or both |
| 3 deadline | seller (or buyer, per the flag) | that same party; valid only when the accepting block's blue score is at least the deadline |

The transaction is valid only if (a) the outputs to the parties the role may pay hold at least the escrowed amount less 1,000,000 atoms (0.01 IMN), (b) the outputs to anyone else hold no more than the transaction's other inputs, so escrowed coins reach the allowed parties or the fee and nowhere else, (c) any token the escrow holds goes to those parties in full, and (d) the transaction spends no other escrow coin.

### 8.6 Offers
An output with script version 5 is an *offer*. Its script is not a hash but the terms themselves, 114 bytes: the maker's Ed25519 public key (32), the asset given and the asset wanted (33 each: a zero byte and 32 zero bytes for IMN, or 1 and a token ID), and a price as two 8-byte big-endian numbers `price_num` and `price_den`, both positive. The two assets differ. Taking `n` units of the given asset costs `ceil(n * price_num / price_den)` units of the wanted one.

An input spending an offer carries one of:

- **Take:** `[0]`, then two 2-byte indices: the output that pays the maker, and the output holding the rest of the offer (`0xffff` for none). No signature is needed. The transaction is valid only if (a) the payment output is locked to the maker's ordinary address and carries at least the cost of what is taken, (b) a rest output, if named, has exactly the offer's script, holds less of the given asset than the offer did and more than none, and for a token offer holds at least the offer's IMN, and (c) no output is named by two offers in the same transaction. What is taken is the offer's amount less the rest. An offer of IMN whose coin holds a token cannot be taken. When a token offer is taken in full, the payment output must also carry the IMN the offer coin held, on top of the price when the price is in IMN.
- **Cancel:** `[1]` and a 64-byte signature by the maker over the ordinary signing hash of that input. Nothing else is required.

Because the terms are in the clear, nodes list every unspent offer as an order book (`/api/v1/offers`).

### 8.7 Subscriptions
An address of type `Subscription` (type byte 6) locks IMN to the hash of a 120-byte script: the customer's and merchant's Ed25519 public keys (32 each, different), the amount per period in atoms (8), the period in blue score (8), the blue score from which the next collection is due (8), and a 32-byte reference chosen by the merchant. Amount and period are positive. The address hash is `Blake3-derive-key("IMN 2026 subscription script", script)`. A subscription coin carries no token.

An input spending such a coin carries the script, a role byte and a 64-byte signature over the ordinary signing hash of that input:

- **Collect** (role 0, signed by the merchant, followed by a 2-byte output index or `0xffff`): valid only when the accepting block's blue score is at least `next_due`. If an output is named, it must be locked to the same script with `next_due` increased by one period, carry no token, and hold at least the coin's value less the amount per period less 1,000,000 atoms; no output may be named by two collections. If none is named, the coin may hold at most the amount per period plus 1,000,000 atoms.
- **Cancel** (role 1, signed by the customer): valid at any time, with no other condition.

### 8.8 Vaults
An address of type `Vault` (type byte 7) locks coins to `Blake3-derive-key("IMN 2026 vault script", script)` of a 72-byte script: an everyday Ed25519 key (32), a different backup key (32) and a positive delay in blue score (8). A *pending withdrawal* (type byte 8) locks coins to `Blake3-derive-key("IMN 2026 vault withdrawal", terms)` of 73 bytes: the destination's script version (1) and hash (32), the blue score from which it may complete (8), and the backup key (32).

- **Start** (vault coin; script, `[0]`, the everyday key's signature, the pending terms, a 2-byte output index): valid only if the terms name the vault's backup key and complete no earlier than the accepting block's blue score plus the delay, and the named output is locked to those terms, carries the coin's token unchanged and holds at least the coin's value less 1,000,000 atoms.
- **Complete** (pending coin; terms, `[0]`, a 2-byte output index; no signature): valid only from the terms' blue score on, and only if the named output is locked to the destination, carries the token unchanged and holds at least the coin's value less 1,000,000 atoms.
- **Recover** (either coin; script or terms, `[1]`, the backup key's signature): valid at any time, with no other condition.

No output may be named by two withdrawals in one transaction.

### 8.9 Swap offers
An output with script version 9 is a *swap offer*: IMN for sale against a token on another chain. Its script is the terms, 104 bytes: the maker's Ed25519 public key (32), the other chain's ID (8), the token's contract address (20), the address the maker is paid at on that chain (20), a price `price_num` and `price_den` (8 each, positive; `n` atoms cost `ceil(n * price_num / price_den)` of the token's smallest units) and the least sold in one fill, in atoms (8). The chain ID, token and payment address are not zero.

An input spending a swap offer carries only a 64-byte signature by the maker over the ordinary signing hash of that input. Nothing else is checked: the chain cannot see the other chain, so the maker fills an offer themselves, by spending it into a hash lock (§8.4) for the buyer and putting the rest back under the same terms. Nodes list unspent swap offers at `/api/v1/swap-offers`.

The other chain's side is the `ImnSwap` contract. A buyer calls `lockForImn`, which locks the payment for the maker's payment address under the SHA-256 hash of the buyer's secret and records the buyer's IMN key and the blue score from which the maker may take back the IMN lock. Both sides derive the IMN lock's address from the buyer's key, the maker's key, the hash and that blue score. The buyer's refund time on the other chain must be later than the IMN lock's by a margin the maker chooses, so the maker can always claim the payment after the buyer claims the IMN and so reveals the secret.

### 8.10 One output, one contract
Within one transaction, an output named by an offer take (as payment or rest), a vault withdrawal or a subscription collection may be named by no other of these, and does not count towards an escrow's allowed destinations (it counts as paid elsewhere). Otherwise one payment could settle two contracts: an offer "paid" with its maker's own vault withdrawal, which anyone may complete, or an escrow released with the seller's own money.

### 8.11 Quantum-safe addresses
An address of type 1 (`PostQuantumHash`) locks coins to `tagged-hash("IMN 2026 quantum-safe keys", Ed25519 public key (32) ++ SQIsign public key (83))`. SQIsign is the round-3 NIST submission (2026-09-01) at level I, as implemented by the `sqisign-rs` crate 0.6.1. A spend's signature script is `Ed25519 key ++ SQIsign key ++ Ed25519 signature (64) ++ SQIsign signature (200)`, 379 bytes; both signatures are over the input's ordinary signing hash, and both must verify (Ed25519 under the strict check). The coins therefore stay safe while either scheme holds. Both keys come from one 32-byte seed: the Ed25519 key is `Blake3-derive-key("IMN 2026 quantum-safe Ed25519 key", seed)`, the SQIsign key is generated from a ChaCha20 stream seeded with `Blake3-derive-key("IMN 2026 quantum-safe SQIsign key", seed)`.

SQIsign signatures are not strongly unforgeable; this changes a transaction's hash but never its ID, which excludes signature scripts. A SQIsign verification costs a few milliseconds, so it is checked last, and as relay policy (not consensus) a transaction may spend at most 16 quantum-safe inputs, each client (IP address or IPv6 /64, over HTTP and over P2P separately, this machine included) may have at most 50 quantum-safe inputs checked per second with a burst of 100, a transaction is checked for room in the mempool before any signature is verified, and a peer relaying a bad quantum-safe signature is penalised heavily. A node remembers inputs whose signature verified (ordinary and quantum-safe addresses only, whose result cannot depend on the blue score) and checks a new block's signatures on several cores; neither can change an outcome.

### 8.12 Keys
Nodes do not create, store or receive private keys. Wallets sign locally; the reference wallet uses a WebAssembly build of the same transaction code the node runs. A wallet's key is derived from a 12-word BIP-39 recovery phrase as `Blake3-derive-key("IMN 2026 wallet key 0", BIP-39 seed)`. Its quantum-safe address (8.11) uses the seed `Blake3-derive-key("IMN 2026 wallet quantum key 0", wallet private key)`, so anything that restores the wallet restores that address too.
