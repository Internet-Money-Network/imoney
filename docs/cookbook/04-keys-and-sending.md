# 4. Keys and sending

Sending is four steps, the same in every language:

1. Ask a node which coins the sender has.
2. Build a transaction that spends some of them.
3. Sign it with the sender's key, on the sender's machine.
4. Hand the signed transaction to any node.

The key never leaves step 3. A node cannot spend for you, and cannot be tricked into it.

## Make an address

```bash
cargo run -p imoney-cookbook --example new_address
```

```
Address:     imntest:qqzh3r96a2v3454gxue2a59l85nvrup6mllm6xmr84jenq7e2fd6ccalehw
Private key: 3f1c…
```

In code, with the `imoney-core` and `ed25519-dalek` crates:

```rust
use ed25519_dalek::SigningKey;
use imoney_core::{Address, AddressType, Network};

let key = SigningKey::generate(&mut rand::rngs::OsRng);
let address = Address::from_public_key(Network::Testnet, AddressType::PubKeyHash, key.verifying_key().as_bytes());
println!("{}", address);                 // imntest:q…
let private_key = key.to_bytes();        // 32 bytes: store these, encrypted
```

To check an address a user typed, parse it. Parsing verifies the checksum, so a mistyped
address is refused instead of sending money nowhere:

```rust
let recipient = Address::decode("imntest:qqzh3r96a2v…")?;
```

## Send a payment

```bash
cargo run -p imoney-cookbook --example send -- ./devnet-data/miner-key.hex imntest:qqzh3r96a2v… 2.5
```

```
imntest:qr48j0… has 413 spendable coins
Sent 2.50000000 IMN, fee 0.00010000 IMN
Transaction 491e5f3437bf119045f09ed68f16e03e28cfdae5e7a0829d1b333ad7611b0c7b
```

The whole of it, from [`send.rs`](../../examples/cookbook/examples/send.rs):

```rust
let node = Node::new(&node_url());
let coins = node.spendable_coins(&address_of(&key))?;                    // step 1

let tx = Transaction::build_invoice_payment(                             // steps 2 and 3
    &key, NETWORK, &recipient, amount_atoms, FEE_ATOMS, coins, None, invoice_id)?;

let tx_id = node.broadcast(&tx)?;                                        // step 4
```

`build_invoice_payment` chooses enough coins to cover the amount and the fee, pays the
recipient, sends the change back to the sender and signs every input. `Node` is thirty lines
in [`lib.rs`](../../examples/cookbook/src/lib.rs): step 1 is `GET /address/…/utxos` and step 4
is:

```
POST /api/v1/tx/broadcast
{ "transaction": { … the transaction as JSON … } }
→ { "success": true, "tx_id": "491e5f34…" }
```

A refusal comes back with the reason, for example:

```json
{ "success": false, "tx_id": null, "error": "Transaction error: Transaction has no inputs" }
```

## The fee

The fee is whatever the inputs hold beyond what the outputs pay. A node relays a payment that
offers at least **10 atoms per byte**. An ordinary payment is about 240 bytes, so 2,400 atoms
is the floor and the examples pay 10,000 (0.0001 IMN) to stay well clear of it. A payment
that spends many coins is larger and needs more.

When blocks are full, miners take the payments offering the most per byte first.

## Naming an invoice

Add an invoice ID and the payment says which order it settles:

```bash
cargo run -p imoney-cookbook --example send -- ./devnet-data/miner-key.hex imntest:qqzh3r… 2.5 order-1001
```

The recipient then finds it with `GET /api/v1/invoice/order-1001?address=…` (chapter 3).

## Payment requests

To ask someone to pay, give them the address with the amount and invoice attached. Wallets
read this form from a link or a QR code:

```
imntest:qqzh3r96a2v…?amount=2.5&invoice=order-1001
```

## In a browser

The wallet at `/wallet` does all of this in the page: the Rust code is compiled to
WebAssembly ([`crates/imoney-wasm`](../../crates/imoney-wasm)), so keys are generated and
payments are signed in the browser, and only the signed transaction is sent to a node. Its
`build_payment` function takes the same inputs as the Rust one. Read
[`apps/imoney-wallet/index.html`](../../apps/imoney-wallet/index.html) for a complete worked
client.

## Things that catch people out

- **"Insufficient funds" right after mining.** A reward cannot be spent for 20 blocks.
  `spendable` in the coin list tells you which coins are ready.
- **Two payments in quick succession.** Until the first is in a block, the node still lists
  the coins it spent, so a second payment is built from the same coins. If it is identical
  to the first it *is* the first (same ID, sent once); if it differs it is refused as a
  conflict. Wait for a block between payments from one address, or keep several coins and
  choose different ones.
- **The same coin spent twice.** The second payment is refused as a conflict. This is the
  ledger working: a coin is spent once.
- **Wrong network.** A key makes a different-looking address on each network
  (`imntest:` / `imn:`), and a payment signed for one is invalid on the other.

Next: [accepting payments](05-accepting-payments.md).
