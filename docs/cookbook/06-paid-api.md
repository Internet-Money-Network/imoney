# 6. A paid API

A service that charges per request, and a program that pays for what it uses with nobody in
the loop. No accounts, no API keys, no card, no payment processor: the invoice from chapter 5
is the whole mechanism.

## Run it

With the private node from chapter 2 running and `IMN_NODE` set, start the service. It needs
only an address to be paid at:

```bash
cargo run -p imoney-cookbook --example paid_api_server -- imntest:qqzh3r96a2v…
```

In another terminal, run a client that has some coins:

```bash
cargo run -p imoney-cookbook --example paid_api_client -- ./devnet-data/miner-key.hex
```

```
Quoted 0.01000000 IMN, invoice api-4792-1
Paid in transaction eda385be202fd74d97e9129038d6843d240ca43dddb0d2e25344064adcabd2ec
Answer after 0.0s: {"data":"the answer you paid for"}
```

## The conversation

```
client                                   service                         service's node
  │  GET /quote                             │                                   │
  │ ───────────────────────────────────────▶│                                   │
  │  { invoice_id, address, price_atoms }   │                                   │
  │ ◀───────────────────────────────────────│                                   │
  │                                                                             │
  │  signed payment naming the invoice  ───────── to any node ─────────────────▶│
  │                                                                             │
  │  GET /data?invoice=api-4792-1           │  GET /invoice/api-4792-1?address= │
  │ ───────────────────────────────────────▶│ ─────────────────────────────────▶│
  │                                         │  seen_atoms >= price ?            │
  │  200 the data   (or 402 until paid)     │ ◀─────────────────────────────────│
  │ ◀───────────────────────────────────────│                                   │
```

The service answers `402 Payment Required` until its own node reports the invoice paid. `402`
is the status code HTTP reserved for exactly this.

## The service

All of the payment logic in
[`paid_api_server.rs`](../../examples/cookbook/examples/paid_api_server.rs) is this:

```rust
let (seen_atoms, _included, _final) = node.invoice_paid(invoice_id, &address)?;
if seen_atoms < PRICE_ATOMS {
    // 402 Payment Required
} else if !served.insert(invoice_id.to_string()) {
    // 410 Gone: this invoice was already used
} else {
    // 200 with the data
}
```

Three decisions are worth copying:

- **It only honours invoices it issued.** Otherwise anyone could present an old paid invoice
  ID of their own choosing.
- **One payment buys one answer.** It remembers which invoices it has served. A real service
  keeps that in a database, so a restart does not hand out free answers.
- **It holds no key.** A break-in at the service cannot move the money it has earned.

## How settled is enough?

The example serves at `seen`, which is why the answer came back at once. That is the right
trade for a request worth a fraction of a cent: the worst case is one free answer.

For something more valuable, wait for `included` (about five seconds) or `final`. Or charge in
advance: let the client pay for a hundred requests at once, wait for `included` once, and
count them down.

## The client

From [`paid_api_client.rs`](../../examples/cookbook/examples/paid_api_client.rs):

```rust
let quote = /* GET /quote */;
if price_atoms > MOST_IT_WILL_PAY_ATOMS {
    return Err(/* too expensive */);
}
let tx = Transaction::build_invoice_payment(&key, NETWORK, &address, price_atoms, FEE_ATOMS,
                                            coins, None, Some(invoice_id))?;
node.broadcast(&tx)?;
// then GET /data?invoice=… until it stops answering 402
```

**Give a program that spends money a limit.** The check against `MOST_IT_WILL_PAY_ATOMS` is
two lines and it is the difference between a bug and an empty wallet. Keep only as much in
such a key as you are willing to lose, and top it up from a key that is kept elsewhere.

## Where this goes

The same four steps work for anything a program can ask another for: a data feed, a
computation, a file, a minute of some resource. Because a payment costs a hundredth of a cent
and settles in seconds, charging for single requests is practical, which it is not with cards.

## Things that catch people out

- **The fee is extra.** The client pays the price plus about 0.0001 IMN to the network.
- **Many small payments make many small coins** for the service. Merge them now and then
  (chapter 9).
- **Quote expiry.** A real service should expire unpaid quotes and say so in the quote, so
  its list of issued invoices does not grow for ever.

Next: [shared wallets](07-shared-wallets.md).
