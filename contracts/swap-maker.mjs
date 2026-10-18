// The seller's side of a swap offer: sells listed IMN for a token on another chain, unattended.
//
// A buyer locks the payment in the ImnSwap contract with `lockForImn`, naming the seller's
// payment address, a hash of their secret, their IMN key and the blue score until which the
// IMN must stay theirs to claim. This program:
//   1. checks each such lock against the seller's offers: the right token, enough paid for at
//      least the offer's smallest amount, and deadlines that leave the seller time to claim;
//   2. fills the offer by locking that much IMN for the buyer under the same hash;
//   3. once the buyer claims the IMN, which publishes the secret on the IMN chain, uses the
//      secret to claim the payment before the buyer could take it back.
// A lock it turns down is simply left alone: the buyer takes it back after its deadline.
//
//     IMN_NODE=http://127.0.0.1:18556 EVM_RPC=https://mainnet.base.org SWAP_CONTRACT=0x… \
//     IMN_KEY_FILE=imn.key EVM_KEY_FILE=evm.key node swap-maker.mjs
//
// The EVM key pays the gas for claims; it needs a little ether and nothing else. The IMN key is
// the one that made the offers. State is kept in swap-maker-state.json next to this file.
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { Contract, JsonRpcProvider, Wallet } from 'ethers';
import init, * as imn from '../apps/imoney-wallet/pkg/imoney_wasm.js';

const BLOCK_SECONDS = 5; // one blue score, roughly
// The buyer must leave at least this long to claim the IMN, and the seller at least this long
// after that to claim the payment.
const MIN_BUYER_BLOCKS = 720; // about an hour
const MAX_BUYER_BLOCKS = 720 * 24;
const SELLER_MARGIN_SECONDS = 6 * 3600;

export const SWAP_ABI = [
  'event ImnOrder(bytes32 indexed id, bytes32 imnKey, uint64 imnRefundAfter)',
  'event Claimed(bytes32 indexed id, bytes32 secret)',
  'function locks(bytes32) view returns (address sender, address recipient, address token, uint256 amount, bytes32 hash, uint64 refundAfter, bool settled)',
  'function claim(bytes32 id, bytes32 secret)',
  'function lockForImn(address recipient, bytes32 hash, uint64 refundAfter, address token, uint256 amount, bytes32 imnKey, uint64 imnRefundAfter) payable returns (bytes32)',
];

export async function loadWasm() {
  await init({ module_or_path: readFileSync(new URL('../apps/imoney-wallet/pkg/imoney_wasm_bg.wasm', import.meta.url)) });
  return imn;
}

/// One maker. `step()` does one round of work and returns what it did, for logs and tests.
export class SwapMaker {
  constructor({ node, testnet = true, imnKey, swap, chainId, fromBlock = 0, state = {} }) {
    Object.assign(this, { node, testnet, imnKey, swap, chainId });
    this.me = JSON.parse(imn.keys_from_private_key(imnKey, testnet));
    this.state = { fromBlock, orders: {}, ...state };
  }

  async api(path, body) {
    const res = await fetch(`${this.node}/api/v1/${path}`, body ? { method: 'POST', headers: { 'Content-Type': 'application/json' }, body } : undefined);
    if (!res.ok) throw new Error(`${path}: ${res.status}`);
    return res.json();
  }

  async broadcast(signedJson) {
    const result = await this.api('tx/broadcast', JSON.stringify({ transaction: JSON.parse(signedJson).transaction }));
    if (!result.success) throw new Error(result.error || 'rejected');
    return result;
  }

  async step() {
    const done = [];
    const score = (await this.api('info')).virtual_blue_score;
    const provider = this.swap.runner.provider ?? this.swap.runner;
    const head = await provider.getBlockNumber();
    const now = (await provider.getBlock('latest')).timestamp;

    // 1 and 2: new orders
    for (const event of await this.swap.queryFilter(this.swap.filters.ImnOrder(), this.state.fromBlock, head)) {
      const id = event.args.id;
      if (this.state.orders[id]) continue;
      const verdict = await this.consider(id, event.args.imnKey.slice(2), Number(event.args.imnRefundAfter), score, now);
      this.state.orders[id] = verdict;
      done.push({ id, ...verdict });
    }
    this.state.fromBlock = head + 1;

    // 3: claim payments whose secret the buyer has published
    for (const [id, order] of Object.entries(this.state.orders)) {
      if (order.status !== 'filled') continue;
      const secret = await this.publishedSecret(order.lockAddress);
      if (secret) {
        await (await this.swap.claim(id, '0x' + secret)).wait();
        order.status = 'paid';
        done.push({ id, status: 'paid' });
      } else if (score >= order.refundAfter && (await this.api(`address/${order.lockAddress}/utxos`)).length) {
        // The buyer never claimed: take the IMN back
        const [coin] = await this.api(`address/${order.lockAddress}/utxos`);
        await this.broadcast(imn.swap_lock_spend(this.imnKey, this.testnet, order.lockHex, JSON.stringify(coin), ''));
        order.status = 'taken back';
        done.push({ id, status: 'taken back' });
      }
    }
    return done;
  }

  async consider(id, buyerKey, refundAfter, score, now) {
    const lock = await this.swap.locks(id);
    if (lock.settled) return { status: 'skipped', why: 'already settled' };
    if (refundAfter < score + MIN_BUYER_BLOCKS || refundAfter > score + MAX_BUYER_BLOCKS) return { status: 'skipped', why: 'IMN deadline out of range' };
    if (Number(lock.refundAfter) < now + (refundAfter - score) * BLOCK_SECONDS + SELLER_MARGIN_SECONDS) return { status: 'skipped', why: 'payment deadline too soon' };

    const offers = (await this.api(`swap-offers?chain_id=${this.chainId}&token=${lock.token}`)).filter(
      (o) => o.maker_key === this.me.public_key_hex && o.pay_to.toLowerCase() === lock.recipient.toLowerCase(),
    );
    const amount = BigInt(lock.amount);
    for (const offer of offers) {
      // The most IMN the payment covers at this offer's price
      let atoms = (amount * BigInt(offer.price_den)) / BigInt(offer.price_num);
      if (atoms > BigInt(offer.atoms) - 600_000n) continue; // more than this offer holds
      if (atoms < BigInt(offer.min_atoms) || atoms === 0n) continue;
      const target = JSON.parse(imn.swap_lock(buyerKey, this.me.public_key_hex, lock.hash.slice(2), BigInt(refundAfter), this.testnet));
      const signed = imn.swap_offer_fill(this.imnKey, this.testnet, JSON.stringify(offer), atoms, buyerKey, lock.hash.slice(2), BigInt(refundAfter));
      await this.broadcast(signed);
      return { status: 'filled', atoms: atoms.toString(), lockAddress: target.address, lockHex: target.terms_hex, refundAfter };
    }
    return { status: 'skipped', why: 'no offer fits' };
  }

  // The secret the buyer revealed by claiming the IMN lock, if they have
  async publishedSecret(lockAddress) {
    for (const item of (await this.api(`address/${lockAddress}/history`)).items) {
      if (item.kind !== 'sent') continue;
      const tx = await this.api(`tx/${item.tx_id}`);
      if (tx.revealed_secrets?.length) return tx.revealed_secrets[0];
    }
    return null;
  }
}

// Run as a program
if (import.meta.url === `file://${process.argv[1].replace(/\\/g, '/')}` || process.argv[1]?.endsWith('swap-maker.mjs')) {
  const env = (name) => process.env[name] ?? (() => { throw new Error(`set ${name}`); })();
  await loadWasm();
  const provider = new JsonRpcProvider(env('EVM_RPC'));
  const signer = new Wallet(readFileSync(env('EVM_KEY_FILE'), 'utf8').trim(), provider);
  const stateFile = new URL('./swap-maker-state.json', import.meta.url);
  const saved = existsSync(stateFile) ? JSON.parse(readFileSync(stateFile, 'utf8')) : {};
  const maker = new SwapMaker({
    node: process.env.IMN_NODE ?? 'http://127.0.0.1:18556',
    testnet: process.env.IMN_MAINNET !== '1',
    imnKey: readFileSync(env('IMN_KEY_FILE'), 'utf8').trim(),
    swap: new Contract(env('SWAP_CONTRACT'), SWAP_ABI, signer),
    chainId: Number((await provider.getNetwork()).chainId),
    fromBlock: saved.fromBlock ?? (await provider.getBlockNumber()),
    state: saved,
  });
  for (;;) {
    try {
      for (const line of await maker.step()) console.log(new Date().toISOString(), JSON.stringify(line));
      writeFileSync(stateFile, JSON.stringify(maker.state, null, 2));
    } catch (err) {
      console.error(new Date().toISOString(), err.message);
    }
    await new Promise((resolve) => setTimeout(resolve, 15_000));
  }
}
