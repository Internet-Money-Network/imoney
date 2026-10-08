# @imoney/sdk

JavaScript and TypeScript SDK for [Internet Money (IMN)](https://github.com/Internet-Money-Network/imoney):
invoices, payment levels, a drop-in checkout window and QR codes. It talks to an Internet Money
node's HTTP API, ideally your own.

> Test network only. The protocol can still change before a main network exists.

```bash
npm install @imoney/sdk
```

```js
import { IMoneyClient } from '@imoney/sdk';

const client = new IMoneyClient({ nodeUrl: 'http://127.0.0.1:18556' });

// An invoice the payer's wallet signs into the payment, so it is matched to this order
const invoice = client.createInvoice({ address: 'imntest:q…', amountImn: '12.5' });
console.log(invoice.uri); // show as text or as a QR code: qrSvg(invoice.uri)

// Resolves when enough has been paid at the level you ask for
const status = await client.waitForPayment(invoice, { level: 'included' });
```

Payment levels:

| Level | Meaning | Typical wait |
| :--- | :--- | :--- |
| `seen` | The node has received the payment. It can still be replaced. | under a second |
| `included` | Accepted into the ledger. | about 5 seconds |
| `final` | Buried under the number of blocks the node requires. | set by the node |

In a browser, `client.openCheckoutModal({ merchantAddress, amountImn, onSuccess })` shows a
ready-made payment window. A script-tag build is in `dist/imoney.js` (global `IMoney`).

Also included: `getBalance`, `getUtxos`, `getHistory`, `getTxStatus`, `getInvoice`,
`broadcastTransaction`, `subscribeAddressPayments`, and the helpers `imnToAtoms`, `atomsToImn`,
`paymentUri`, `parsePaymentUri`, `newInvoiceId`, `isValidInvoiceId`, `qrSvg`.

The SDK does not hold keys or sign. Signing is done by the payer's wallet; to sign in your own
code use the Rust crate `imoney-core` or the WebAssembly build in the main repository.

**Confirm payments on your server**, by asking your own node, before releasing goods. A result
that only a browser has seen can be faked by that browser.

Licence: MIT or Apache-2.0.
