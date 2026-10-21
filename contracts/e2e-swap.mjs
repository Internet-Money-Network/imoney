// One whole atomic swap across both chains: Alice's IMN for Bob's ether.
//
// Needs a private Internet Money node (see docs/cookbook/02-private-network.md) and the
// cookbook's swap_cli built:
//
//     cargo build -p imoney-cookbook --example swap_cli
//     IMN_NODE=http://127.0.0.1:28556 node e2e-swap.mjs <alice's IMN key file> ../target/debug/examples/swap_cli
//
// Ethereum is an in-process test chain, so nothing here touches a real network.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { BrowserProvider, ContractFactory, ZeroAddress, formatEther, parseEther } from 'ethers';
import { network } from 'hardhat';

const [aliceImnKey, cli] = process.argv.slice(2);
const node = process.env.IMN_NODE ?? 'http://127.0.0.1:18556';
const imn = (...args) => JSON.parse(execFileSync(cli, args, { encoding: 'utf8' }));
const api = async (path) => (await fetch(`${node}/api/v1/${path}`)).json();
const balance = async (address) => (await api(`address/${address}/balance`)).balance_atoms;
const until = async (what, check) => {
  for (let i = 0; i < 120; i++) {
    if (await check()) return;
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error(`timed out waiting for ${what}`);
};
const AMOUNT_ATOMS = 300_000_000n; // 3 IMN
const PRICE = parseEther('0.01');

// The Ethereum side
const chain = (await network.connect()).provider;
const provider = new BrowserProvider(chain, undefined, { cacheTimeout: -1 });
const [aliceEth, bobEth] = [await provider.getSigner(0), await provider.getSigner(1)];
const { abi, bytecode } = JSON.parse(readFileSync('out/ImnSwap.json', 'utf8'));
const swap = await new ContractFactory(abi, bytecode, aliceEth).deploy();
await swap.waitForDeployment();

// Bob's key on the Internet Money side
const bobImnKey = randomBytes(32).toString('hex');
const bob = imn('pubkey', bobImnKey);

// 1. Alice picks the secret and shares only its hash
const secret = randomBytes(32);
const hash = createHash('sha256').update(secret).digest('hex');
console.log('1. Alice picks a secret; its hash is', hash);

// 2. Alice locks IMN for Bob, refundable after about an hour of blocks
const lock = imn('lock', aliceImnKey, bob.public_key, hash, String(AMOUNT_ATOMS), '720');
await until('the IMN lock to be in a block', async () => BigInt(await balance(lock.address)) === AMOUNT_ATOMS);
console.log('2. Alice locked 3 IMN at', lock.address);

// 3. Bob sees it and locks ether for Alice under the same hash, with the earlier deadline
const deadline = (await provider.getBlock('latest')).timestamp + 1800;
const receipt = await (await swap.connect(bobEth).lock(aliceEth.address, '0x' + hash, deadline, ZeroAddress, PRICE, { value: PRICE })).wait();
const lockId = swap.interface.parseLog(receipt.logs.at(-1)).args.id;
console.log('3. Bob locked', formatEther(PRICE), 'ether for Alice');

// 4. Alice claims the ether, which publishes the secret
const aliceBefore = await provider.getBalance(aliceEth.address);
await (await swap.connect(bobEth).claim(lockId, '0x' + secret.toString('hex'))).wait(); // anyone may send it; Alice is paid
assert.equal((await provider.getBalance(aliceEth.address)) - aliceBefore, PRICE);
console.log('4. Alice has the ether');

// 5. Bob reads the secret from the Ethereum event and claims the IMN
const [claimed] = await swap.queryFilter(swap.filters.Claimed(lockId));
const revealed = claimed.args.secret.slice(2);
const claim = imn('claim', bobImnKey, lock.script, lock.tx_id, String(AMOUNT_ATOMS), revealed);
await until('Bob to hold the IMN', async () => BigInt(await balance(bob.address)) > 0n);
console.log('5. Bob read the secret from Ethereum and claimed the IMN in', claim.tx_id);

assert.equal(BigInt(await balance(lock.address)), 0n);
assert.equal(BigInt(await balance(bob.address)), AMOUNT_ATOMS - 500_000n); // less the network fee
assert.equal(await provider.getBalance(swap.target), 0n);
// The IMN claim shows the same secret to anyone who looks
assert.equal((await api(`tx/${claim.tx_id}`)).revealed_secrets[0], revealed);
console.log('Done: 3 IMN for', formatEther(PRICE), 'ether, with nobody holding either in between.');
process.exit(0);
