# Internet Money (`IMN`) roadmap

Where the project stands and what is left before a public test network and, later, a main
network. This file is kept honest: an item is ticked only when it exists and has been run.

## What the network is for

Payments that a shop can accept without trusting anyone else. Three things make that work:

- **Invoice numbers in the payment.** The payer's wallet signs the order's invoice number into
  the transaction, so a payment is matched to an exact order.
- **Honest confirmation levels.** A payment is reported as *seen* (reached the node, under a
  second), *included* (accepted into the ledger, about 5 seconds) or *final* (deep enough for
  the value at stake). A shop chooses the level per order.
- **A node the shop runs itself.** One block every five seconds and a 100 KB block limit keep a
  verifying node small enough for a cheap server.

## Done

- [x] **Protocol.** Canonical binary encoding, Ed25519 signatures that commit to the network,
      UTXO ledger, GHOSTDAG (k = 8) ordering, enforced difficulty adjustment, coinbase maturity,
      12-hour finality, fee split between the miner and the node that served the payer,
      multi-signature addresses (m of up to 16 keys).
- [x] **Proof of work.** Money Printer: FishHash with a network-specific seed, checked against
      Iron Fish's implementation. Nodes verify from a light cache.
- [x] **Node.** `redb` storage with atomic block application, mempool with fee policy and
      limits, REST and WebSocket API, binary peer-to-peer protocol with sync, orphan handling,
      peer discovery and bans, optional pruning, private test networks (`--devnet`).
- [x] **Mining.** Reference CPU miner, OpenCL GPU miner, and a Stratum bridge for other mining
      software ([docs/STRATUM.md](docs/STRATUM.md)).
- [x] **Wallet.** Browser wallet that creates keys and signs locally, with a 12-word phrase.
- [x] **Merchant tools.** JavaScript SDK with invoices, payment levels, checkout window and
      locally drawn QR codes; end-to-end demo shop; WooCommerce plugin, run against a current
      WooCommerce with its block checkout (order paid only by a payment naming its invoice).
- [x] **Explorer.** Live block graph, hashrate, difficulty, supply, block, transaction and
      address lookup, address history.
- [x] **Operations.** Docker and systemd files, node guide, load-test tool with reference
      results, integration guide for exchanges, wallets and pools, release builds from tags.

## Before a public test network

- [ ] **Dataset size decided.** The current test network uses a 32 MB dataset so a CPU can
      mine. A test network meant for GPU miners needs the full 4.6 GB size; changing it resets
      the chain.
- [ ] **WooCommerce plugin on a production-like site.** It has been run end to end on a local
      WordPress; its five-minute background check and a real mail and MySQL setup have not.
- [ ] **SDK published to npm.**
- [ ] **Three or more seed nodes in different places**, at least one outside a home connection.
- [ ] **A soak run of several days** on those nodes, watching memory, disk and sync.
- [ ] **A first tagged release** with binaries.

## Before a main network

- [ ] **Independent review** of consensus and of the proof of work.
- [ ] **Multi-signature in the wallet.** The protocol and the Rust crate have it; the browser
      wallet and the WebAssembly build do not.
- [ ] **Hardware-wallet support.**
- [ ] **Block index out of memory.** Headers and DAG data are held in memory, about 1 KB per
      block.
- [ ] **Less contention in the node.** One lock guards the ledger, and ancestry checks walk the
      DAG. Fine at measured loads (see [docs/LOAD-TESTING.md](docs/LOAD-TESTING.md)); it is the
      ceiling.
- [ ] **Long fuzzing runs.** Fuzz targets for the decoders, signature checks and the
      peer-to-peer protocol exist (`fuzz/`) and run briefly in CI; nobody has run them for days.
- [ ] **A genesis block with a realistic starting difficulty**, and a fair, announced launch.

## Later, after a main network

- **A bridge to an EVM chain** (Base first), so wrapped IMN can trade on existing exchanges
  there. It would be run by named signers, capped, deposit-only at first, with users paying
  their own gas and a stated bridge fee funding development. Not started.
- **The wallet as an installable app**, with the bridge as a screen in it.

## Not planned

- Smart contracts.
- A premine, a developer fee or an allocation of any kind.
