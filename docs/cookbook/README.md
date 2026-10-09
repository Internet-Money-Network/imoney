# Internet Money cookbook

How to build on Internet Money, one task at a time. Each chapter explains what is going on,
then gives something you can run. Every command and program here was run against a private
test network before it was written down.

Read chapter 1 once. After that, jump to whatever you need.

| | Chapter | You will be able to |
| :--- | :--- | :--- |
| 1 | [How it works](01-how-it-works.md) | Explain coins, addresses, blocks and confirmations |
| 2 | [A private network on your machine](02-private-network.md) | Start a node that mines its own chain, to test against |
| 3 | [Reading the chain](03-reading-the-chain.md) | Look up balances, payments, history and blocks over HTTP |
| 4 | [Keys and sending](04-keys-and-sending.md) | Make an address, build and sign a payment, send it |
| 5 | [Accepting payments](05-accepting-payments.md) | Match a payment to an order and decide when to trust it |
| 6 | [A paid API](06-paid-api.md) | Charge per request, and write a program that pays by itself |
| 7 | [Shared wallets](07-shared-wallets.md) | Hold funds that need two of three keys to move |
| 8 | [Mining](08-mining.md) | Get work from a node, submit a block, run through a pool |
| 9 | [Reference](09-reference.md) | Look up units, limits, every endpoint and common errors |

## The examples

The runnable programs are in [`examples/cookbook`](../../examples/cookbook). They are short,
they use only a node's public HTTP API and the `imoney-core` crate, and they are compiled and
linted with the rest of the repository, so they cannot quietly go stale.

```bash
cargo run -p imoney-cookbook --example new_address
```

They talk to `http://127.0.0.1:18556` unless you set `IMN_NODE`:

```bash
export IMN_NODE=http://127.0.0.1:28556
```

| Example | Chapter | What it does |
| :--- | :--- | :--- |
| `new_address` | 4 | Makes a key and shows its address |
| `send` | 4 | Sends a payment, optionally for an invoice |
| `paid_api_server` | 6 | A service that answers once an invoice is paid |
| `paid_api_client` | 6 | A program that gets a quote, pays it and collects |
| `multisig` | 7 | Funds a 2-of-3 address and spends from it |

## Other documents

- [SPECIFICATION.md](../../SPECIFICATION.md): the rules, exactly. Read it to write another
  implementation or to review this one.
- [INTEGRATION.md](../INTEGRATION.md): the checklist for exchanges, wallets and pools.
- [RUNNING-A-NODE.md](../RUNNING-A-NODE.md): operating a node that stays up.
- [STRATUM.md](../STRATUM.md): the mining protocol pools and miners speak.

The network is a test network. Rules can still change, and its coins have no value.
