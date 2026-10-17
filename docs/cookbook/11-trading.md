# 11. Trading on the chain

Internet Money has a trading layer built into the protocol. There is no exchange to deposit
into and no company in the middle: an offer is a coin held by the chain under fixed terms,
and taking it is a single transaction that pays the maker and delivers the goods together,
or does nothing at all.

## How an offer works

- **Placing.** A maker locks coins (IMN or a token) in an offer whose terms say what it gives,
  what it wants back, the price, and who gets paid. The terms are stored in the open, so every
  node can list them.
- **Taking.** Anyone can take an offer, in full or in part, without asking the maker. Every node
  checks that the same transaction pays the maker at least the price for what is taken. If it
  doesn't, the transaction is refused.
- **Partial takes.** Whatever isn't taken goes straight back into an identical offer, at the
  same price for the next taker.
- **Cancelling.** Only the maker can cancel, and gets everything back.
- **Races.** If two people try to take the same offer at once, the first to reach a block wins
  and the other's transaction is refused. Nobody pays for a trade that didn't happen.

The chain never matches orders or decides prices. Takers choose which offers to take, and
makers choose their price.

## The market price

The order book is public, so the price of a token in IMN can be read from the chain itself: the
best offer to sell and the best offer to buy. No outside price feed is involved. The wallet's
Trade tab shows both.

This is a price people offer, not one anyone guarantees, and on a thin book it moves with a
single offer.

## Reading the order book

```bash
curl "$N/api/v1/offers?give=<token id>&want=imn"     # offers selling the token, cheapest first
curl "$N/api/v1/offers?give=imn&want=<token id>"     # offers buying it
```

```json
[ { "transaction_id": "…", "index": 0, "maker": "imntest:qzx0l8…",
    "give": "2406ad8b…", "give_amount": 60000, "want": "imn",
    "price_num": 1000000, "price_den": 100, "price": 10000.0,
    "terms_hex": "…", "value_atoms": 0, "token": { "token_id": "2406ad8b…", "amount": 60000 } } ]
```

Taking `n` units of `give` costs `ceil(n * price_num / price_den)` units of `want`. Amounts are in
smallest units: for a token with 2 decimals, 60000 is 600.00.

`terms_hex`, `value_atoms` and `token` describe the offer coin exactly, so a wallet can build a
take from the listing alone. If a node misreported them, the take would simply be refused.

## In code

```rust
// Offer 600.00 BEAN (60,000 units, 2 decimals) at 0.01 IMN each:
// 1,000,000 atoms per 100 units
let offer = Transaction::build_offer(&maker, NETWORK, Some(bean), 60_000, None, 1_000_000, 100, fee, maker_coins)?;

// Take 250.00 of it
let take = Transaction::take_offer(&taker, NETWORK, offer_coin, 25_000, fee, taker_coins)?;

// Cancel what is left
let cancel = Transaction::cancel_offer(&maker, NETWORK, rest_coin, fee, maker_coins)?;
```

In the browser, the WebAssembly functions `offer_create`, `offer_take` and `offer_cancel` do the
same, and the wallet's **Trade** tab puts them behind a form.

## Trading with other chains

Offers settle within Internet Money: IMN against any token, or token against token. For
another chain's coins, such as ether, use a hash lock swap. That works without a middleman
too, but needs the two people to exchange a hash first.

## Things that catch people out

- **The price is per smallest unit.** The wallet converts to "IMN per token" for you; code must
  account for decimals itself.
- **Rounding favours the maker.** The cost of a take is rounded up to the next unit.
- **An offer is only listed once it is in a block.** For the five seconds before that, nobody
  can see it or take it.
- **Offers are public.** Everyone can see what you offer and at what price, and everyone can
  take it. Don't place an offer you aren't happy to have filled.
