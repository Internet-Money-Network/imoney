# 5. Accepting payments

To be paid you need an address and a way to answer one question: *has this order been paid,
and how sure am I?* You do not need a key on the server, an account anywhere, or anyone's
permission.

## The pattern

1. For each order, make an **invoice ID** that is unique to it. Your order number is fine.
2. Show the customer a payment request: your address, the amount, the invoice ID.
3. Ask **your own node** whether that invoice has been paid in full at the level you require.
4. Release the goods.

One address serves every order, because the invoice ID tells payments apart. Keep the key for
that address offline; the server only ever needs the address.

## Step 2: the payment request

```
imntest:qqzh3r96a2v…?amount=2.5&invoice=order-1001
```

Shown as a link or a QR code, a wallet fills in everything from it.

## Step 3: has it been paid?

```bash
curl "http://127.0.0.1:28556/api/v1/invoice/order-1001?address=imntest:qqzh3r96a2v…"
```

```json
{ "payments": [ { "tx_id": "491e5f34…", "amount_atoms": 250000000, "confirmations": 3, "level": "included" } ],
  "seen_atoms": 250000000, "included_atoms": 250000000, "final_atoms": 0,
  "final_confirmations": 60, "network_alert": false }
```

Pick the total for the level you require and compare it with the price in atoms:

```rust
let (seen, included, finalised) = node.invoice_paid("order-1001", &address)?;
if included >= price_atoms {
    // paid: it is in a block
}
```

Three rules make this safe:

- **Compare atoms with `>=`.** A customer may pay in two parts, or overpay. The totals add up
  every payment that names the invoice.
- **Match on the invoice, never on the balance.** "The balance went up by 2.5" cannot tell
  order 1001 from order 1002.
- **Decide on the server.** A page in the customer's browser can show progress, but the
  customer controls that page. Only your server asking your node counts.

## Choosing the level

| Wait for | When |
| :--- | :--- |
| `seen_atoms` | The item is cheap and instant delivery matters more than the rare loss. A sender can still replace a payment that is not in a block. |
| `included_atoms` | The usual choice. About five seconds. |
| `final_atoms` | The amount is large or the delivery cannot be undone. About five minutes with the default of 60 confirmations. |

`final_atoms` stays at zero while `network_alert` is true. The node raises that flag when the
chain has recently reorganised deeply or it sees a heavier chain it refuses to follow. Treat
it as "wait".

Run your node with `--final-confirmations N` to set what "final" means for you.

## Waiting without hammering the node

Poll every second or two while a checkout is open, and stop when it is paid or expires. Or
open the WebSocket for your address (chapter 3) and ask again whenever a message arrives.
Either way, the HTTP answer is what you act on.

Keep checking after you have released the goods if you released at `seen` or `included`: a
payment that later vanishes is something your bookkeeping should notice.

## In a web page

The JavaScript SDK in [`packages/imoney-sdk`](../../packages/imoney-sdk) wraps this. Build it
with `npm install && npm run build` in that directory.

```js
import { IMoneyClient } from './dist/index.mjs';

const client = new IMoneyClient({ nodeUrl: 'http://127.0.0.1:28556' });

const invoice = client.createInvoice({
  address: 'imntest:qqzh3r96a2v…',
  amountImn: '2.5',
  invoiceId: 'order-1001',
});
console.log(invoice.uri);        // imntest:qqzh3r…?amount=2.5&invoice=order-1001

const status = await client.waitForPayment(invoice, {
  level: 'included',
  onProgress: (s) => console.log('seen so far:', s.seen_atoms),
});
console.log('paid by', status.payments.map((p) => p.tx_id));
```

`openCheckoutModal` shows a ready-made checkout window with the amount, a QR code and live
status. It is a convenience for the customer; your server still confirms with its own node
before shipping.

## Without writing code

The WooCommerce plugin in [`plugins/imoney-payments-for-woocommerce`](../../plugins/imoney-payments-for-woocommerce)
does all of the above for a WordPress shop: one invoice per order, a checkout window, and a
server-side check against the shop's node before the order is marked paid.

## Things that catch people out

- **Reusing an invoice ID.** Two orders with one ID share their payments. Make IDs unique.
- **Underpayment.** The totals will be short. Show the customer what is still owed; the same
  invoice ID on a second payment tops it up.
- **Asking someone else's node.** Whoever runs the node you ask can tell you anything. A node
  that verifies everything itself runs on a small server; see
  [RUNNING-A-NODE.md](../RUNNING-A-NODE.md).
- **Refunds.** There is no "reverse". A refund is a new payment from you to the customer.

Next: [a paid API](06-paid-api.md).
