# Internet Money (`IMN`) Protocol Specification

**Document Version:** 1.0.0  
**Status:** Working Draft  

---

## 1. Abstract

Internet Money (`IMN`) is an open-source decentralized cryptocurrency based on a directed acyclic graph of blocks (BlockDAG) ordered by the GHOSTDAG consensus protocol. It is engineered to provide sub-second transaction visibility with 5-second deterministic settlement, while preserving absolute ASIC resistance and decentralized mining distribution via the memory-hard **Money Printer** Proof-of-Work algorithm.

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
Algorithms that rely solely on arithmetic compute (e.g. SHA-256, kHeavyHash) inevitably succumb to ASIC takeover because custom silicon can multiply logic density by orders of magnitude compared to general-purpose hardware.

The **Money Printer** algorithm forces memory bandwidth saturation:
1. **Dataset:** A pseudorandom dataset of 64-byte items generated from an epoch seed derived from the consensus Virtual Selected Parent Chain.
2. **Memory Iterations:** Every hash evaluation executes 32 pseudo-random memory lookups into the dataset, followed by arithmetic mixing across 64-bit integer lanes.
3. **Hardware Target:** Standard consumer gaming GPUs equipped with 4 GB to 16 GB of VRAM. An ASIC manufacturer must package commodity HBM/GDDR memory, eliminating the economic edge over retail graphics cards.

### 3.2 Instant Node Verification
Full nodes and mobile wallets do not require high-performance GPU mining rigs. Verification evaluates the resulting hash against the 256-bit difficulty target in sub-millisecond execution using CPU vector instructions.

---

## 4. Monetary Policy & Emission Curve

### 4.1 Smooth 4-Year Halving (No Opening Premine)
* **Genesis Block:** Zero premine.
* **Launch Block Subsidy:** $5.00000000\text{ IM}$ per block ($500\text{M}$ atoms).
* **Era Duration:** $25,228,800$ blocks (exactly 4 Julian years).

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
| **0** | 0 – 4 | 0 – 25,228,799 | 5.00000000 IM | 126,144,000 IM | 126,144,000 IM |
| **1** | 4 – 8 | 25,228,800 – 50,457,599 | 2.50000000 IM | 63,072,000 IM | 189,216,000 IM |
| **2** | 8 – 12 | 50,457,600 – 75,686,399 | 1.25000000 IM | 31,536,000 IM | 220,752,000 IM |
| **3** | 12 – 16 | 75,686,400 – 100,915,199 | 0.62500000 IM | 15,768,000 IM | 236,520,000 IM |
| **4+** | 16+ | 100,915,200+ | **0.31250000 IM** | ~1,971,000 IM / yr | Uncapped (~1% tail) |

---

## 5. Difficulty Adjustment Algorithm (DAA)

Difficulty retargeting uses a rolling window of 144 blocks ($720\text{ seconds} \approx 12\text{ minutes}$).
The next target $T_{next}$ is computed as:
$$T_{next} = T_{curr} \times \frac{\text{clamp}(t_{actual}, 0.5 \cdot t_{target}, 2.0 \cdot t_{target})}{t_{target}}$$
Damping clamps prevent wild difficulty swings caused by transient multi-pool hashrate switching.
