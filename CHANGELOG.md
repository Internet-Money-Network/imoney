# Changelog

Notable changes to Internet Money. Nothing has been released yet: everything below is on the
`main` branch, on the test network, and the protocol can still change.

## Unreleased

### Added
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
- **Signatures are checked strictly.** A public key of small order let one fixed signature
  pass for any message, so coins sent to an address made from such a key could be taken by
  anyone. Ordinary keys were never affected. Found by the fuzzer in CI.
- The WooCommerce gateway was not offered at all in the block-based checkout.

## Before this file

See the git history. In outline: the protocol and node were rebuilt from a single-node demo
into a multi-node network (canonical encoding, GHOSTDAG, enforced difficulty, UTXO ledger with
reorganisation, peer-to-peer sync, FishHash proof of work, 12-hour finality, fee split,
invoices with payment levels, browser-signing wallet, pruning, load testing).
