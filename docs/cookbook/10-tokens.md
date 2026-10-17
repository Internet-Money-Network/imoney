# 10. Tokens

Anyone can issue a token on Internet Money: loyalty points, a game currency, a community coin,
tickets, or a share in something. There is no contract to write. A token is created by one
transaction and moves like IMN, and every node checks that nobody creates or loses any along
the way.

## How tokens work

- **A coin can carry a token.** Besides its IMN, any coin may carry an amount of one token.
  Fees are always paid in IMN, so holding a little IMN is how you move tokens.
- **A token's ID can't be copied.** The ID is derived from the transaction that created the
  token. The name and ticker are labels anyone can reuse, so wallets and shops should go by
  the ID. The explorer shows it on every token page.
- **Tokens are never lost by accident.** In a transfer, every token's amounts in and out must
  match exactly. A wallet that forgets a token is refused instead of quietly destroying it.
  To destroy tokens on purpose, send them to the burn script.
- **Minting is a coin too.** If the issuer keeps the *mint authority*, it sits in their wallet
  like any other coin. Whoever holds it can mint more, hand it to someone else, put it in a
  shared wallet so two of three people must agree, or give it up so the supply can never grow.
- **Amounts are whole numbers.** A token with 2 decimals counts in hundredths, so 2.50 is 250
  units. Pick decimals when issuing; they can't change.

## Run it

```bash
cargo run -p imoney-cookbook --example token -- ./devnet-data/miner-key.hex
```

```
Issued token b7bb197dee64…
  {"name":"Coffee points","ticker":"BEAN","decimals":2,"initial_supply":100000,"mintable_at_issue":true, …}
The shop holds: [{"amount":250,"ticker":"BEAN", …}]
The issuer holds: [{"amount":149750,"mint_authority":true, …}]
```

The steps, from [`token.rs`](../../examples/cookbook/examples/token.rs):

```rust
// Issue 1,000.00 BEAN with the right to mint more
let info = TokenInfo { name: "Coffee points".into(), ticker: "BEAN".into(), decimals: 2, url: String::new() };
let issue = Transaction::build_token_issue(&key, NETWORK, info, 100_000, true, fee, imn_coins)?;
let token_id = issue.issued_token().unwrap();

// Pay a shop 2.50 BEAN for order-1
let pay = Transaction::build_token_transfer(&key, NETWORK, &shop, token_id, 250, fee, all_coins, Some("order-1"))?;

// Mint 500.00 more, keeping the authority
let mint = Transaction::build_token_mint(&key, NETWORK, token_id, 50_000, false, fee, all_coins)?;
```

The transfer and mint builders need all of the sender's coins, including the ones carrying
tokens, because that's where the tokens are. The IMN-only builders from chapter 4 skip coins
that carry tokens, so an ordinary payment never touches them.

## Reading tokens

```bash
curl $N/api/v1/address/$A/tokens
```

```json
[ { "token_id": "b7bb197d…", "amount": 250, "mint_authority": false,
    "name": "Coffee points", "ticker": "BEAN", "decimals": 2 } ]
```

```bash
curl $N/api/v1/token/b7bb197dee64d7a4482ba984fcee4b9631718ef49f8ad03ad80a410a5152fd93
```

```json
{ "token_id": "b7bb197d…", "name": "Coffee points", "ticker": "BEAN", "decimals": 2, "url": "",
  "issued_in": "9be49cc2…", "issuer": "imntest:qqkg5x…", "initial_supply": 100000, "mintable_at_issue": true }
```

Coins in `/address/{address}/utxos` and outputs in `/tx/{id}` carry a `token` field when they
hold one. In JavaScript, `client.getTokens(address)` and `client.getToken(id)` do the same.

## Accepting tokens as payment

A token payment carries an invoice ID exactly like an IMN payment. To check it, read the
transaction and add up the outputs to your address that carry the token you expect:

```js
const tx = await client.getTxStatus(txId);
const paid = tx.outputs
  .filter((o) => o.address === myAddress && o.token?.token_id === EXPECTED_TOKEN && !o.token.mint_authority)
  .reduce((sum, o) => sum + o.token.amount, 0);
```

Always compare the **token ID**, never the ticker. Anyone can issue a token called "USD".

## Trading tokens without an exchange

A hash lock (the swap building block in the IMN specification, §8.4) can hold tokens. That
makes any token tradeable for IMN, for another token, or for coins on Bitcoin- and
Ethereum-style chains, with nobody holding anything in between. The one rule: a lock that
holds tokens must also hold enough IMN for the fee of whoever claims it, which
`build_token_transfer_with_atoms` attaches.

## In the wallet and the explorer

- **Wallet:** the **Tokens** tab lists what you hold, sends tokens, issues new ones, and mints
  more if you hold the authority.
- **Explorer:** token amounts appear on transactions and addresses. Search a token ID to see
  its page.

## Things that catch people out

- **No IMN, no transfer.** Every token transfer pays its fee in IMN. Keep a little in any
  address that holds tokens.
- **Decimals are fixed at issue.** 1,000 units of a token with 3 decimals is 1.000.
- **Names are not unique.** Check the ID, especially before accepting a token as payment.
- **Issuing is a public act.** A token issued for others to buy can carry legal obligations
  for the issuer, depending on where you are. The chain doesn't decide that; take advice.
- **Pruned nodes forget old issuances.** A node started with `--prune` can't describe tokens
  issued before its pruning point, though balances are unaffected. Run an unpruned node to
  serve token pages.
