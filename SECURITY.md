# Security

Internet Money is test-network software. Nothing here holds real value yet, and the protocol
has not had an independent review. Reports are still very welcome: problems found now are cheap
to fix.

## Reporting a vulnerability

Please do not open a public issue for anything that could be used to steal coins, create coins,
split the network, or crash or take over a node.

Report it privately by email to **security@internetmoneynetwork.org**.

Include what you can of:

- what the problem is and what an attacker gains;
- the steps or a test that shows it;
- the commit you tested.

You will get an acknowledgement, and the fix will credit you unless you ask otherwise. There is
no bug bounty: the project has no premine and no funds to pay one.

## What counts

- Consensus: anything that makes two honest nodes disagree, accepts an invalid block or
  transaction, or reverses history past the finality depth.
- Money: creating coins, spending coins without the key, or spending them twice.
- Proof of work: a way to find valid hashes much more cheaply than the algorithm intends.
- Node: remote crashes, memory or disk exhaustion by a peer or an API caller, remote code
  execution.
- Wallet, SDK and WooCommerce plugin: key exposure, or marking an order paid when it is not.

## What does not

- Attacks that need a majority of the network's hashpower. The test network's hashpower is
  tiny; this is expected.
- Problems only present in the test network's settings (small dataset, short maturity).

## Supported versions

Only the `main` branch.
