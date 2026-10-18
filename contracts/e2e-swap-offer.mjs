// A whole swap offer, end to end: a seller lists IMN for a token, a buyer pays on the other
// chain, the seller's swap-maker fills it, the buyer claims the IMN and the maker the payment.
//
// Needs a private Internet Money node mining to the seller's key (see
// docs/cookbook/02-private-network.md) and the wallet's wasm built:
//
//     IMN_NODE=http://127.0.0.1:28556 node e2e-swap-offer.mjs <seller's IMN key file>
//
// The other chain is an in-process test chain, so nothing here touches a real network.
import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { BrowserProvider, Contract, ContractFactory } from 'ethers';
import ganache from 'ganache';
import { SWAP_ABI, SwapMaker, loadWasm } from './swap-maker.mjs';

const imn = await loadWasm();
const node = process.env.IMN_NODE ?? 'http://127.0.0.1:18556';
const api = async (path) => (await fetch(`${node}/api/v1/${path}`)).json();
const broadcast = async (signedJson) => {
  const res = await fetch(`${node}/api/v1/tx/broadcast`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ transaction: JSON.parse(signedJson).transaction }),
  });
  const result = await res.json();
  assert.ok(result.success, result.error);
};
const until = async (what, check) => {
  for (let i = 0; i < 240; i++) {
    const value = await check();
    if (value) return value;
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error(`timed out waiting for ${what}`);
};

// The other chain, with the swap contract and a token the buyer holds 1,000,000 units of
const chain = ganache.provider({ logging: { quiet: true }, chain: { hardfork: 'london' } });
const provider = new BrowserProvider(chain, undefined, { cacheTimeout: -1 });
const [buyerEvm, sellerEvm] = [await provider.getSigner(0), await provider.getSigner(1)];
const build = (name) => JSON.parse(readFileSync(`out/${name}.json`, 'utf8'));
const deployed = async (name, ...args) => {
  const contract = await new ContractFactory(build(name).abi, build(name).bytecode, buyerEvm).deploy(...args);
  await contract.waitForDeployment();
  return contract;
};
const swapAddress = await (await deployed('ImnSwap')).getAddress();
const token = await deployed('TestToken', false);
const tokenAddress = await token.getAddress();
const chainId = Number((await provider.getNetwork()).chainId);

// 1. The seller lists 10 IMN at 1,000 token units per IMN, at least 1 IMN at a time
const sellerKey = readFileSync(process.argv[2], 'utf8').trim();
const seller = JSON.parse(imn.keys_from_private_key(sellerKey, true));
// (first taking down any offers an earlier run left, which also tries cancelling)
for (const old of await api(`swap-offers?chain_id=${chainId}`)) {
  if (old.maker_key === seller.public_key_hex) await broadcast(imn.swap_offer_cancel(sellerKey, true, JSON.stringify(old)));
}
await until('old offers to be gone', async () => (await api(`swap-offers?chain_id=${chainId}`)).length === 0);
const utxos = await api(`address/${seller.address}/utxos`);
await broadcast(
  imn.swap_offer_create(sellerKey, true, BigInt(chainId), tokenAddress, sellerEvm.address, 1000n, 100_000_000n, 100_000_000n, 1_000_000_000n, JSON.stringify(utxos)),
);
const [offer] = await until('the offer to be listed', async () => {
  const book = await api(`swap-offers?chain_id=${chainId}`);
  return book.length ? book : null;
});
assert.equal(offer.atoms, 1_000_000_000);
assert.equal(offer.pay_to.toLowerCase(), sellerEvm.address.toLowerCase());
console.log('1. Listed 10 IMN at 1,000 units each:', `${offer.transaction_id}:${offer.index}`);

// The seller's program, watching the other chain from here on
const maker = new SwapMaker({ node, imnKey: sellerKey, swap: new Contract(swapAddress, SWAP_ABI, sellerEvm), chainId, fromBlock: await provider.getBlockNumber() });

// 2. The buyer picks a secret and pays 3,000 units for 3 IMN, naming their IMN key
const buyerKey = randomBytes(32).toString('hex');
const buyer = JSON.parse(imn.keys_from_private_key(buyerKey, true));
const secret = randomBytes(32);
const hash = createHash('sha256').update(secret).digest('hex');
const score = (await api('info')).virtual_blue_score;
const imnRefundAfter = score + 1440; // about two hours for the buyer to claim
const refundAfter = (await provider.getBlock('latest')).timestamp + 1440 * 5 + 8 * 3600;
await (await token.connect(buyerEvm).approve(swapAddress, 3000)).wait();
const swap = new Contract(swapAddress, SWAP_ABI, buyerEvm);
await (await swap.lockForImn(sellerEvm.address, '0x' + hash, refundAfter, tokenAddress, 3000, '0x' + buyer.public_key_hex, imnRefundAfter)).wait();
console.log('2. The buyer locked 3,000 units for the seller');

// 3. The maker sees it and fills 3 IMN into a lock for the buyer
const [filled] = await maker.step();
assert.equal(filled.status, 'filled', filled.why);
assert.equal(filled.atoms, '300000000');
console.log('3. The maker filled 3 IMN at', filled.lockAddress);

// 4. The buyer finds the lock from what they already know, and claims it with the secret
const lock = JSON.parse(imn.swap_lock(buyer.public_key_hex, seller.public_key_hex, hash, BigInt(imnRefundAfter), true));
assert.equal(lock.address, filled.lockAddress);
const [coin] = await until('the IMN lock to be in a block', async () => {
  const coins = await api(`address/${lock.address}/utxos`);
  return coins.length ? coins : null;
});
await broadcast(imn.swap_lock_spend(buyerKey, true, lock.terms_hex, JSON.stringify(coin), secret.toString('hex')));
await until('the buyer to hold the IMN', async () => (await api(`address/${buyer.address}/balance`)).balance_atoms > 0);
console.log('4. The buyer claimed the IMN, publishing the secret');

// 5. The maker reads the secret from the IMN chain and claims the payment
await until('the maker to claim the payment', async () => (await maker.step()).some((d) => d.status === 'paid'));
assert.equal(await token.balanceOf(sellerEvm.address), 3000n);
assert.equal((await api(`address/${buyer.address}/balance`)).balance_atoms, 300_000_000 - 20_000);
const [rest] = await api(`swap-offers?chain_id=${chainId}`);
assert.equal(rest.atoms, 1_000_000_000 - 300_000_000 - 20_000);
console.log('5. The seller has the 3,000 units; 7 IMN are still on offer');
console.log('Done: 3 IMN for 3,000 token units across two chains, with nobody holding either in between.');
process.exit(0);
