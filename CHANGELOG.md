# Changelog

Notable changes to Internet Money. Nothing has been released yet: everything below is on the
`main` branch, on the test network, and the protocol can still change.

## Unreleased

### Added
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
- The WooCommerce gateway was not offered at all in the block-based checkout.

## Before this file

See the git history. In outline: the protocol and node were rebuilt from a single-node demo
into a multi-node network (canonical encoding, GHOSTDAG, enforced difficulty, UTXO ledger with
reorganisation, peer-to-peer sync, FishHash proof of work, 12-hour finality, fee split,
invoices with payment levels, browser-signing wallet, pruning, load testing).
