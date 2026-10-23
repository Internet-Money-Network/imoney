# Changelog

Notable changes to Internet Money. Nothing has been released yet: everything below is on the
`main` branch, on the test network, and the protocol can still change.

## Unreleased

### Added
- **A maintenance fund: 5% of each block's subsidy for the first three years** (18,921,600
  blocks), paid by the ledger to one address fixed in the rules and stopping by itself. Miners
  claim the other 95%, and all transaction fees. Total supply and the emission schedule are
  unchanged. A consensus change: it arrives with a test network reset.
- **The public test network was restarted as testnet-5**, with quantum-safe addresses: type 1
  locks coins to an Ed25519 key and a SQIsign key, and spending needs both signatures. A new
  genesis (2026-10-23) keeps out nodes from before quantum-safe spends, which would skip them and
  part from the network (round-4 finding R4-1). Wallets use the quantum-safe address as their
  main one; checking SQIsign signatures is budgeted per client (R4-2). Delete the data directory
  except `miner-key.hex` before upgrading. See `deploy/TESTNET-5-RESET.md`.
- **The public test network was restarted as testnet-4**, because the difficulty rule now
  takes a block's time as the latest stamp among it and the ten selected-chain blocks before
  it (a miner backdating its blocks could otherwise slow the chain), and with the pen-test
  fixes of October 2026: a subscription address holds one coin, mint authorities are counted,
  dust is not relayed. New genesis block: delete the data directory except `miner-key.hex`
  before upgrading. See `deploy/TESTNET-4-RESET.md`.
- **The public test network was restarted as testnet-3**, carrying the consensus changes
  below (tokens, offers, swap offers, escrow, subscriptions, vaults) and the October 2026
  security fixes, including a past median time taken from the selected chain. It has a new
  genesis block: delete the data directory except `miner-key.hex` before upgrading.
- **Higher minimum fee: 1,000 atoms per byte** (was 10). An ordinary payment now costs about
  0.0025 IMN, still well under a cent at any price likely for years, and filling every block
  for a day costs about 17,000 IMN instead of 170. Node policy, not a consensus rule, so it can
  be lowered by a release without a reset.
- **Swap offers: IMN for stablecoins on other chains.** A seller lists IMN against a token on
  another chain (USDC on Base, say) in the order book, at a price and with a payment address.
  A buyer locks the payment in the `ImnSwap` contract with `lockForImn`; the seller's
  `swap-maker` program checks it and locks the IMN for the buyer under the same hash; the
  buyer claims the IMN, which reveals the secret the seller then uses to claim the payment.
  If the seller never responds the buyer takes the payment back after its deadline. New:
  `GET /api/v1/swap-offers`. A consensus rule.
- **Vaults.** Savings that leave only after a waiting period, to the address named when the
  withdrawal started. A backup key, kept offline, can stop a withdrawal during the wait or
  move the savings at any time, so a stolen everyday key gains a thief nothing quickly. A
  consensus rule.
- **Subscriptions.** A customer sets money aside that a merchant may collect from at most a set
  amount once per period; whatever is left stays locked until the next period, and the
  customer can cancel at any time and take it back. A consensus rule.
- **A trading layer in the protocol.** A maker places an offer: coins held by the chain under
  public terms (what is given, what is wanted, the price, who is paid). Anyone can take all or
  part of it in one transaction that pays the maker, or nothing happens; the rest stays on
  offer, and only the maker can cancel. Nodes list open offers as an order book
  (`/api/v1/offers`), and the wallet's Trade tab shows the book, the market price read from it,
  and forms to place, take and cancel offers. A consensus rule.
- **Escrow.** An escrow address holds coins for a buyer and a seller with an arbiter for
  disputes. The buyer can only release to the seller, the seller can only refund the buyer,
  the arbiter can only pay one or both of them, and after a deadline one named side collects
  alone. Nodes check where the money goes, not just who signed. A consensus rule.
- **Native tokens.** Anyone can issue a token on the chain: a name, a ticker, a supply, and
  optionally the right to mint more, held as a coin that can be passed on, shared in a
  multisig, or given up to fix the supply. Tokens are conserved exactly in every transfer,
  fees are paid in IMN, and tokens work with invoices and hash locks, so a token can be
  traded for IMN or another token with no exchange. New: `GET /api/v1/token/{id}` and
  `GET /api/v1/address/{address}/tokens`. This changes the block format: the test network
  will be reset when it is deployed.
- **Hash locks**, the building block of atomic swaps: coins that the recipient can claim with
  a secret, or the sender can take back after a deadline. With the same lock on another chain,
  two people can trade IMN for another asset with no exchange and no custodian. A consensus
  rule: every node must run this version before anyone uses it.
- **Refunds.** A node reports the address each invoice payment came from (`payer_address`) and
  the senders of any transaction. A refund is a payment back under the invoice ID
  `<id>.refund`, sent from the merchant's own wallet. The SDK gains `createRefund`,
  `getRefund` and `refundInvoiceId`; the WooCommerce plugin supports the Refund button, with
  part refunds, a QR code to pay from the shop's wallet, and a field for another address.
- **A cookbook** (`docs/cookbook`): nine chapters on how the network works and how to build on
  it, with runnable examples in `examples/cookbook`, including a paid API and a client that
  pays for it by itself.
- **A mining pool.** `imoney-stratum --pool` shares out each matured block reward among miners
  by the work their shares prove, less a fee (1.5% by default), pays out automatically and
  serves a status page.
- **The explorer and wallet are on the website**, reading a public node through `/api`.
- **The GPU miner keeps its dataset on disk**, so only the first start takes minutes. Every
  start checks the card against the CPU before mining; a damaged file is rebuilt.
- **Address history.** `GET /api/v1/address/{address}/history` lists what an address received
  and sent, newest first, with paging. Shown in the explorer and the wallet, and available in
  the SDK as `getHistory`.
- **Multi-signature addresses** (m of up to 16 Ed25519 keys) in the protocol and the Rust crate.
- **OpenCL GPU miner** (`imoney-gpu-miner`), which checks the card against the CPU before mining.
- **Stratum bridge** (`imoney-stratum`) for pools and other mining software, with the protocol
  written up in `docs/STRATUM.md`. The GPU miner can mine through it (`--stratum`).
- **WooCommerce: block checkout support.** The gateway now appears in WooCommerce's default
  checkout and declares compatibility with its order tables.
- Fuzz targets for the decoders, signature checks, peer protocol and address parsing, run
  briefly in CI.
- Randomized checks of the GHOSTDAG rules against a brute-force definition.
- Release builds: pushing a `v*` tag publishes binaries for Linux, Windows and macOS.
- `SECURITY.md`, `CONTRIBUTING.md`, `TRADEMARKS.md`, an integration guide and this file.

### Changed
- **The GPU miner is about 40% faster**: 26 to 37-39 MH/s on a Radeon RX 7800 XT at full
  dataset size. The kernel now works on 64-bit words and is compiled with the dataset size
  as a constant.
- **Nodes are slower to ban each other.** Only a block without proof of work bans a peer at
  once. Other invalid blocks, which an honest peer on an older version can relay, score a
  little and the score fades with time. A failure of our own disk no longer counts against
  the peer that sent the block.
- **Seed names are looked up again** every ten minutes, and retried when the lookup fails at
  startup. A node that falls far behind on a live connection catches up in batches.
- **Catching up is faster.** A syncing node waits for the disk once per batch of blocks
  instead of once per block: about nine times faster on a fast SSD, more on slow disks.
- **A node's memory no longer grows with the chain.** Only recent blocks are held in memory;
  older ones are read from disk when needed. Existing databases are upgraded when first opened.
- **A faster difficulty rule.** Each block's difficulty now follows from how far the last step
  of its selected chain was off schedule (100-second half life, at most a factor of two per
  step), replacing a 144-block average. It follows a hashrate jump within minutes and recovers
  from a drop in about ten minutes, where the old rule took over an hour. Resets the chain.
- **Transactions are smaller.** The unused lock time, subnetwork ID, gas and per-input sequence
  fields are gone: 44 bytes less for a simple payment (about 240 bytes now), so lower fees.
  A future feature that needs a new field takes a new transaction version. Resets the chain.
- **A block may not name a parent more than the finality depth behind its best parent.** This
  bounds the work a node does to check any block.
- **The proof of work is now called Hallmark** (it was "Money Printer"). It is still FishHash
  with a network-specific seed; the seed's label changed with the name, so the test chain
  was restarted again.
- **The public test network was restarted as testnet-2** with the full-size FishHash dataset
  (4.6 GB), so it is mined with graphics cards. It has a new genesis block; data directories
  from before must be deleted. Private networks (`--devnet`) keep the small dataset.
- **Blocks are stored compressed**, about 40% smaller for blocks full of payments.
- Nodes report their network's dataset sizes, and the miners and the Stratum bridge take them
  from the node instead of assuming them.
- A node refuses to open a database that belongs to a different network.
- **An address has exactly one spelling.** The same address written with a Bech32m checksum
  was accepted before and is now refused. Upper-case addresses, as QR codes produce, are now
  accepted.
- **Coins locked to a script-hash address can now be spent** (by a valid multi-signature
  spend). Before, such outputs could be created but never spent. Every node must upgrade.
- The node's database gains a history index. An existing database keeps working, but has no
  history for blocks accepted before the upgrade.
- The WooCommerce plugin zip now contains a top-level folder, as WordPress expects.

### Fixed
- **The database cache is capped at 256 MB.** The library's default of 1 GB let a node's memory
  climb as its database grew. Found in the soak run. `/api/v1/info` now reports
  `blocks_in_memory`.
- **Signatures are checked strictly.** A public key of small order let one fixed signature
  pass for any message, so coins sent to an address made from such a key could be taken by
  anyone. Ordinary keys were never affected. Found by the fuzzer in CI.
- The WooCommerce gateway was not offered at all in the block-based checkout.

## Before this file

See the git history. In outline: the protocol and node were rebuilt from a single-node demo
into a multi-node network (canonical encoding, GHOSTDAG, enforced difficulty, UTXO ledger with
reorganisation, peer-to-peer sync, FishHash proof of work, 12-hour finality, fee split,
invoices with payment levels, browser-signing wallet, pruning, load testing).
