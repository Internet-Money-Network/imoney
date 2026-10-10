// Tests run against the built CommonJS bundle, with the node's HTTP API replaced by a stub.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { test } from 'node:test';

const require = createRequire(import.meta.url);
const sdk = require('../dist/index.js');
const { IMoneyClient } = sdk;

const ADDRESS = 'imntest:qexampleaddress';

/** Replaces global fetch with a stub that serves invoice statuses from `sequence`, one per call. */
function stubInvoiceApi(sequence) {
  const calls = [];
  let i = 0;
  globalThis.fetch = async (url) => {
    calls.push(String(url));
    const status = sequence[Math.min(i++, sequence.length - 1)];
    return { ok: true, status: 200, statusText: 'OK', json: async () => status };
  };
  return calls;
}

function invoiceStatus(seen, included, final, extra = {}) {
  return {
    invoice_id: 'INV-1',
    address: ADDRESS,
    payments: seen ? [{ tx_id: 'ab'.repeat(32), amount_atoms: seen, confirmations: included ? 1 : 0, level: 'seen' }] : [],
    seen_atoms: seen,
    included_atoms: included,
    final_atoms: final,
    final_confirmations: 60,
    network_alert: false,
    ...extra,
  };
}

test('amounts convert to atoms exactly', () => {
  assert.equal(sdk.imnToAtoms('12.5'), 1_250_000_000);
  assert.equal(sdk.imnToAtoms(0.1), 10_000_000);
  assert.equal(sdk.imnToAtoms('0.00000001'), 1);
  assert.equal(sdk.imnToAtoms(0.1 + 0.2), 30_000_000); // not 30000000.000000004
  assert.equal(sdk.atomsToImn(1_250_000_000), '12.5');
  assert.equal(sdk.atomsToImn(1), '0.00000001');
  assert.equal(sdk.atomsToImn(500_000_000), '5');
  assert.throws(() => sdk.imnToAtoms('1.123456789'));
  assert.throws(() => sdk.imnToAtoms('-1'));
  assert.throws(() => sdk.imnToAtoms('abc'));
});

test('an invoice carries its ID in the payment URI and parses back', () => {
  const client = new IMoneyClient();
  const invoice = client.createInvoice({ address: ADDRESS, amountImn: 12.5, invoiceId: 'INV-1042' });
  assert.deepEqual(invoice, {
    invoiceId: 'INV-1042',
    address: ADDRESS,
    amountAtoms: 1_250_000_000,
    amountImn: '12.5',
    uri: `${ADDRESS}?amount=12.5&invoice=INV-1042`,
  });
  assert.deepEqual(sdk.parsePaymentUri(invoice.uri), { address: ADDRESS, amountImn: '12.5', invoiceId: 'INV-1042' });
  assert.deepEqual(sdk.parsePaymentUri(ADDRESS), { address: ADDRESS, amountImn: undefined, invoiceId: undefined });

  const generated = client.createInvoice({ address: ADDRESS, amountImn: '1' });
  assert.match(generated.invoiceId, /^inv-[0-9a-f]{16}$/);
  assert.notEqual(generated.invoiceId, client.createInvoice({ address: ADDRESS, amountImn: '1' }).invoiceId);

  assert.throws(() => client.createInvoice({ address: ADDRESS, amountImn: 1, invoiceId: 'has space' }));
  assert.throws(() => client.createInvoice({ address: ADDRESS, amountImn: 0 }));
  assert.throws(() => sdk.parsePaymentUri(`${ADDRESS}?amount=1&invoice=bad%20id`));
});

test('QR codes are generated locally as SVG', () => {
  const svg = sdk.qrSvg(`${ADDRESS}?amount=12.5&invoice=INV-1042`);
  assert.match(svg, /^<svg[^>]*>/);
  assert.match(svg, /<path /);
  assert.notEqual(svg, sdk.qrSvg(`${ADDRESS}?amount=12.5&invoice=INV-1043`));
});

test('waitForPayment resolves only when the full amount reaches the requested level', async () => {
  const client = new IMoneyClient({ nodeUrl: 'http://node.test' });
  const invoice = client.createInvoice({ address: ADDRESS, amountImn: '0.001', invoiceId: 'INV-1' });
  const calls = stubInvoiceApi([
    invoiceStatus(0, 0, 0),
    invoiceStatus(50_000, 0, 0), // underpaid
    invoiceStatus(100_000, 0, 0), // seen in full
    invoiceStatus(100_000, 100_000, 0), // included
  ]);

  const progress = [];
  const status = await client.waitForPayment(invoice, { level: 'included', pollMs: 5, onProgress: (s) => progress.push(s.seen_atoms) });
  assert.equal(status.included_atoms, 100_000);
  assert.deepEqual(progress, [0, 50_000, 100_000, 100_000]);
  assert.equal(calls[0], `http://node.test/api/v1/invoice/INV-1?address=${encodeURIComponent(ADDRESS)}`);
});

test('waitForPayment at the seen level resolves as soon as the node has the payment', async () => {
  const client = new IMoneyClient();
  const invoice = client.createInvoice({ address: ADDRESS, amountImn: '0.001', invoiceId: 'INV-1' });
  stubInvoiceApi([invoiceStatus(100_000, 0, 0)]);
  const status = await client.waitForPayment(invoice, { level: 'seen', pollMs: 5 });
  assert.equal(status.included_atoms, 0);
});

test('waitForPayment does not treat an included payment as final', async () => {
  const client = new IMoneyClient();
  const invoice = client.createInvoice({ address: ADDRESS, amountImn: '0.001', invoiceId: 'INV-1' });
  // Buried, but the node reports an unsettled network, so final_atoms stays at zero
  stubInvoiceApi([invoiceStatus(100_000, 100_000, 0, { network_alert: true })]);
  await assert.rejects(client.waitForPayment(invoice, { level: 'final', pollMs: 5, timeoutMs: 60 }), /Timed out/);
});

test('waitForPayment can be cancelled and survives a node outage', async () => {
  const client = new IMoneyClient();
  const invoice = client.createInvoice({ address: ADDRESS, amountImn: '0.001', invoiceId: 'INV-1' });

  stubInvoiceApi([invoiceStatus(0, 0, 0)]);
  const abort = new AbortController();
  const waiting = client.waitForPayment(invoice, { pollMs: 5, signal: abort.signal });
  setTimeout(() => abort.abort(), 20);
  await assert.rejects(waiting, /cancelled/);

  let attempts = 0;
  globalThis.fetch = async () => {
    attempts += 1;
    if (attempts < 3) throw new Error('connection refused');
    return { ok: true, status: 200, statusText: 'OK', json: async () => invoiceStatus(100_000, 100_000, 0) };
  };
  const status = await client.waitForPayment(invoice, { pollMs: 5 });
  assert.equal(status.included_atoms, 100_000);
  assert.equal(attempts, 3);
});

test('a refund is a payment request back to the customer under the order\'s refund number', () => {
  const client = new IMoneyClient({ nodeUrl: 'http://node.test' });
  assert.equal(sdk.refundInvoiceId('order-1001'), 'order-1001.refund');
  // An ID too long for the suffix is shortened, and the result is still a valid ID
  const long = 'x'.repeat(64);
  assert.equal(sdk.refundInvoiceId(long).length, 64);
  assert.ok(sdk.isValidInvoiceId(sdk.refundInvoiceId(long)));
  assert.throws(() => sdk.refundInvoiceId('not valid!'));

  const refund = client.createRefund({ invoiceId: 'order-1001', toAddress: ADDRESS, amountImn: '0.5' });
  assert.equal(refund.invoiceId, 'order-1001.refund');
  assert.equal(refund.amountAtoms, 50_000_000);
  assert.equal(refund.uri, `${ADDRESS}?amount=0.5&invoice=order-1001.refund`);
});
