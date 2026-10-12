// Runs the swap contract on an in-process Ethereum chain.
import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { BrowserProvider, ContractFactory, Interface, ZeroAddress, parseEther } from 'ethers';
import ganache from 'ganache';

const artifact = (name) => JSON.parse(readFileSync(`out/${name}.json`, 'utf8'));
const sha256 = (hex) => '0x' + createHash('sha256').update(Buffer.from(hex.slice(2), 'hex')).digest('hex');
const HOUR = 3600;

async function setup() {
  const chain = ganache.provider({ logging: { quiet: true }, chain: { hardfork: 'london' } });
  // Ethers remembers identical requests for a moment; the tests read balances either side of a
  // transaction, so that is switched off
  const provider = new BrowserProvider(chain, undefined, { cacheTimeout: -1 });
  const [alice, bob, carol] = await Promise.all([0, 1, 2].map((i) => provider.getSigner(i)));
  const deploy = async (name, ...args) => {
    const { abi, bytecode } = artifact(name);
    const contract = await new ContractFactory(abi, bytecode, alice).deploy(...args);
    await contract.waitForDeployment();
    return contract;
  };
  const swap = await deploy('ImnSwap');
  const now = async () => (await provider.getBlock('latest')).timestamp;
  const wait = async (seconds) => {
    await chain.request({ method: 'evm_increaseTime', params: [seconds] });
    await chain.request({ method: 'evm_mine', params: [] });
  };
  const secret = '0x' + randomBytes(32).toString('hex');
  /** Locks and returns the new lock's id. */
  const lock = async (from, recipient, { token = ZeroAddress, amount, hash = sha256(secret), after = HOUR }) => {
    const deadline = (await now()) + after;
    const value = token === ZeroAddress ? amount : 0n;
    const receipt = await (await swap.connect(from).lock(recipient, hash, deadline, token, amount, { value })).wait();
    return swap.interface.parseLog(receipt.logs.at(-1)).args.id;
  };
  return { provider, alice, bob, carol, swap, deploy, wait, now, secret, lock };
}

/**
 * The call must be refused with exactly this one of the contract's errors. The test chain
 * reports the refusal as raw data, which is decoded against the contract's own error list.
 */
const errors = new Interface(artifact('ImnSwap').abi);
const reverts = (promise, error) =>
  assert.rejects(promise, (e) => {
    const data = e.data ?? e.info?.error?.data?.result;
    const name = e.revert?.name ?? (data ? errors.parseError(data)?.name : undefined);
    assert.equal(name, error);
    return true;
  });

test('the recipient is paid when anyone presents the secret, and the secret is published', async () => {
  const { provider, alice, bob, carol, swap, secret, lock } = await setup();
  const id = await lock(alice, bob.address, { amount: parseEther('1') });
  assert.equal(await provider.getBalance(swap.target), parseEther('1'));

  const before = await provider.getBalance(bob.address);
  // Carol sends the claim; Bob is still the one paid
  const receipt = await (await swap.connect(carol).claim(id, secret)).wait();
  assert.equal((await provider.getBalance(bob.address)) - before, parseEther('1'));
  assert.equal(await provider.getBalance(swap.target), 0n);
  const claimed = swap.interface.parseLog(receipt.logs[0]);
  assert.equal(claimed.name, 'Claimed');
  assert.equal(claimed.args.secret, secret);

  // Settled once
  await reverts(swap.claim(id, secret), 'AlreadySettled');
  await reverts(swap.refund(id), 'AlreadySettled');
});

test('a wrong secret pays nobody', async () => {
  const { provider, alice, bob, swap, lock } = await setup();
  const id = await lock(alice, bob.address, { amount: parseEther('1') });
  await reverts(swap.claim(id, '0x' + '11'.repeat(32)), 'WrongSecret');
  await reverts(swap.claim('0x' + '22'.repeat(32), '0x' + '11'.repeat(32)), 'UnknownLock');
  assert.equal(await provider.getBalance(swap.target), parseEther('1'));
});

test('the sender gets an unclaimed lock back, but only after the deadline', async () => {
  const { provider, alice, bob, carol, swap, secret, lock, wait } = await setup();
  const id = await lock(alice, bob.address, { amount: parseEther('2') });
  await reverts(swap.refund(id), 'TooEarly');
  await wait(HOUR + 1);

  const before = await provider.getBalance(alice.address);
  await (await swap.connect(carol).refund(id)).wait();
  assert.equal((await provider.getBalance(alice.address)) - before, parseEther('2'));
  // The secret is worthless once the lock has been refunded
  await reverts(swap.claim(id, secret), 'AlreadySettled');
});

test('a claim is still honoured after the deadline if the lock has not been refunded', async () => {
  const { provider, alice, bob, swap, secret, lock, wait } = await setup();
  const id = await lock(alice, bob.address, { amount: parseEther('1') });
  await wait(HOUR + 1);
  await (await swap.claim(id, secret)).wait();
  assert.equal(await provider.getBalance(swap.target), 0n);
});

test('locks are refused when they could not be settled sensibly', async () => {
  const { alice, bob, swap, secret, now } = await setup();
  const hash = sha256(secret);
  const later = (await now()) + HOUR;
  await reverts(swap.lock(ZeroAddress, hash, later, ZeroAddress, 1n, { value: 1n }), 'BadRecipient');
  await reverts(swap.lock(bob.address, hash, (await now()) - 1, ZeroAddress, 1n, { value: 1n }), 'DeadlineInThePast');
  await reverts(swap.lock(bob.address, hash, later, ZeroAddress, 2n, { value: 1n }), 'WrongValue');
  await reverts(swap.lock(bob.address, hash, later, ZeroAddress, 0n, { value: 0n }), 'NothingToLock');
  // An address with no code is not a token
  await reverts(swap.connect(alice).lock(bob.address, hash, later, bob.address, 5n), 'TransferFailed');
});

test('two locks with identical terms are separate', async () => {
  const { provider, alice, bob, swap, secret, lock } = await setup();
  const first = await lock(alice, bob.address, { amount: parseEther('1') });
  const second = await lock(alice, bob.address, { amount: parseEther('1') });
  assert.notEqual(first, second);
  await (await swap.claim(first, secret)).wait();
  assert.equal(await provider.getBalance(swap.target), parseEther('1'));
  await (await swap.claim(second, secret)).wait();
  assert.equal(await provider.getBalance(swap.target), 0n);
});

for (const lenient of [false, true]) {
  test(`tokens are locked, claimed and refunded${lenient ? ' (a token that returns false on failure)' : ''}`, async () => {
    const { alice, bob, swap, deploy, secret, lock, wait, now } = await setup();
    const token = await deploy('TestToken', lenient);

    // Without an approval nothing moves
    await reverts(swap.lock(bob.address, sha256(secret), (await now()) + HOUR, token.target, 500n), 'TransferFailed');

    await (await token.approve(swap.target, 800n)).wait();
    const claimed = await lock(alice, bob.address, { token: token.target, amount: 500n });
    const refunded = await lock(alice, bob.address, { token: token.target, amount: 300n });
    assert.equal(await token.balanceOf(swap.target), 800n);

    await (await swap.claim(claimed, secret)).wait();
    assert.equal(await token.balanceOf(bob.address), 500n);

    await wait(HOUR + 1);
    await (await swap.refund(refunded)).wait();
    assert.equal(await token.balanceOf(alice.address), 1_000_000n - 500n);
    assert.equal(await token.balanceOf(swap.target), 0n);
  });
}

test('a recipient that calls back in during its payment cannot be paid twice', async () => {
  const { provider, alice, swap, deploy, secret, lock } = await setup();
  const attacker = await deploy('Reentrant');
  // Something else is in the contract for the attacker to try to take
  await lock(alice, alice.address, { amount: parseEther('3') });
  const id = await lock(alice, attacker.target, { amount: parseEther('1') });
  await (await attacker.aim(swap.target, id, secret)).wait();

  await (await swap.claim(id, secret)).wait();
  assert.equal(await attacker.reentryWorked(), false);
  assert.equal(await provider.getBalance(attacker.target), parseEther('1'));
  assert.equal(await provider.getBalance(swap.target), parseEther('3'));
});

test('the hash is plain SHA-256 of the 32-byte secret, as on the Internet Money side', async () => {
  // The same vector as imoney-core's `secret_hash`: thirty-two 0x5a bytes
  assert.equal(sha256('0x' + '5a'.repeat(32)), '0x' + createHash('sha256').update(Buffer.alloc(32, 0x5a)).digest('hex'));
  const { alice, bob, swap, lock } = await setup();
  const fixed = '0x' + '5a'.repeat(32);
  const id = await lock(alice, bob.address, { amount: 10n, hash: sha256(fixed) });
  await (await swap.claim(id, fixed)).wait();
});
