# 2. A private network on your machine

Develop against a network that is yours alone. It starts in seconds, mines its own blocks,
gives you as many coins as you like, and can be deleted and restarted at will.

## Start it

Build once:

```bash
cargo build --release -p imoney-node
```

Then run a node on a private network called `dev`, mining by itself:

```bash
./target/release/imoney-node --devnet dev --data-dir ./devnet-data --rpc-bind 127.0.0.1:28556 --p2p-bind 127.0.0.1:28555 --auto-mine
```

What the options do:

| Option | Why |
| :--- | :--- |
| `--devnet dev` | A private network. It uses small proof-of-work tables (32 MB instead of 4.6 GB) and an easy starting difficulty, and only talks to nodes started with the same name, so it can never mix with the public network. |
| `--data-dir ./devnet-data` | Where the chain is kept. Delete the directory to start over. |
| `--rpc-bind`, `--p2p-bind` | Ports other than the defaults, so this node can run beside a real one. |
| `--auto-mine` | The node mines its own blocks on the CPU. |

It prints something like this:

```
[*] Mining Address (local key): imntest:qr48j09gsanjnv52vytlx7t8z3f0yxvsrmz06wqhtd252fep35h4vl6jfpk
[*] Its private key is stored in "./devnet-data/miner-key.hex". Keep that file private.
[+] Mined Block #2 | ... | Miner Balance: 5.00 IMN
```

The first blocks arrive quickly, then the difficulty settles at about one block every five
seconds. Each block pays 5 IMN to the mining address, and a reward becomes spendable after 20
more blocks, so within two minutes you have coins to spend.

`miner-key.hex` is your faucet. The examples take it as their key:

```bash
export IMN_NODE=http://127.0.0.1:28556
cargo run -p imoney-cookbook --example send -- ./devnet-data/miner-key.hex <address> 2.5
```

## Look at it

The node serves a few pages itself:

- `http://127.0.0.1:28556/explorer` shows blocks and payments as they happen.
- `http://127.0.0.1:28556/wallet` is the reference wallet. It makes its own key; send it
  coins from `miner-key.hex` with the `send` example above.

And the API answers at once:

```bash
curl http://127.0.0.1:28556/api/v1/info
```

## A second node

To see blocks travel between nodes, start another with the same network name and point it at
the first:

```bash
./target/release/imoney-node --devnet dev --data-dir ./devnet-data-2 --rpc-bind 127.0.0.1:28566 --p2p-bind 127.0.0.1:28565 --peers 127.0.0.1:28555
```

It downloads the chain and follows along. A payment given to either node reaches the other.

## Starting over

Stop the node and delete its data directory. A private network has no history worth keeping.

## Good to know

- Coins on a private network are worth nothing and exist only on your machine.
- The public test network uses the full-size proof of work, so mining there needs a GPU
  (chapter 8). For development that only wants to read the public chain, point `IMN_NODE` at
  `https://internetmoneynetwork.org`.
- Time matters. A node refuses blocks stamped more than 30 seconds ahead of its own clock,
  so keep the machine's clock set.

Next: [reading the chain](03-reading-the-chain.md).
