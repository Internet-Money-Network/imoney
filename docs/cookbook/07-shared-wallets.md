# 7. Shared wallets

A **multi-signature** address needs several keys to spend: any *m* of up to 16. Two of three
is the common choice. It protects against one stolen key and one lost key at the same time,
and it lets a treasury require two people.

## Run it

```bash
cargo run -p imoney-cookbook --example multisig -- ./devnet-data/miner-key.hex
```

```
2-of-3 address: imntest:qtsnm2xva435jf7t0den4ynqnuqep74x30mh29zhdjjm59tqsvv8u0llkv8
Script to keep: 02032f2ecc5a5d54…
Funded in bfca8ab64a9c6b9c40ba20a39ad37baa8c7bd47afb95ee38bd281d03604a616b
Two holders paid 0.40000000 IMN back out in 7aefb30ac524577d7bc1e11cfd4f626e05c744d41cdf1427f3508c67671ab310
```

## Create the address

Each holder makes their own key and shares only the **public** half.

```rust
use imoney_core::{MultisigScript, Network};

let script = MultisigScript::new(2, &[public_key_a, public_key_b, public_key_c])?;   // 2 of 3
let address = script.address(Network::Testnet);
```

The order of the keys does not matter: the same three keys and threshold always give the same
address.

**Back up the script.** `script.to_bytes()` is the threshold and the public keys. It is needed
to spend, and the address alone does not reveal it. It is not secret, so every holder should
keep a copy.

## Receive

It is an ordinary address. Pay into it, look up its balance, coins and history, and attach
invoices exactly as in the earlier chapters.

## Spend

```rust
// Anyone can prepare the payment. It is worth nothing until enough holders sign.
let mut tx = Transaction::build_multisig_payment(
    &script, Network::Testnet, &recipient, amount_atoms, fee_atoms, coins, None, None)?;

for input in 0..tx.inputs.len() {
    // Each holder, on their own machine:
    let signature_a = tx.multisig_sign(Network::Testnet, input, &key_a);
    let signature_c = tx.multisig_sign(Network::Testnet, input, &key_c);
    // Whoever collects them:
    tx.set_multisig_signatures(input, &script, &[signature_a, signature_c])?;
}
node.broadcast(&tx)?;
```

Signatures do not depend on each other, so holders can sign in any order, at different times,
on machines that are offline. What travels between them is the unsigned transaction and each
holder's signatures.

Every holder should check what they are signing: the recipient, the amount, and that the
change goes back to the shared address. The reference wallet's **Shared** tab shows exactly
that before it signs, and passes a proposal between holders as a block of text.

## Things that catch people out

- **Exactly the threshold.** A 2-of-3 spend carries two signatures. Three are refused.
- **One signature per input.** A transaction spending four coins needs each holder to sign
  four times. The loop above does that.
- **Losing the script.** With all three keys but no script, the funds are still recoverable,
  because the script can be rebuilt from the public keys and the threshold. With only the
  address and fewer keys than you thought, they are not. Keep the script.
- **A slightly larger payment.** Each input carries the script and the signatures, so allow a
  little more fee than for an ordinary payment.

Next: [mining](08-mining.md).
