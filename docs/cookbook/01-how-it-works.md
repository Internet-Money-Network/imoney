# 1. How it works

Everything in the later chapters rests on five ideas. None of them needs mathematics.

## Coins, not accounts

The ledger does not store "address A has 12 IMN". It stores **coins**: individual amounts,
each locked to an address. Your balance is the sum of the coins locked to your addresses.

A **transaction** spends whole coins and creates new ones. To pay 3 IMN from a 5 IMN coin, a
transaction spends the 5 IMN coin and creates two: 3 IMN locked to the recipient and the rest
locked back to you (the *change*). Whatever is not given to an output is the **fee**.

```
spends:   coin of 5.0000 (yours)
creates:  coin of 3.0000 (theirs)
          coin of 1.9999 (yours, change)
fee:      0.0001
```

Three things follow, and they explain most surprises:

- A coin is spent once, completely. Two transactions that spend the same coin conflict, and
  only one can ever be accepted.
- A coin is named by the transaction that created it and its position there:
  `transaction_id:index`. The API calls these *UTXOs* (unspent transaction outputs).
- Receiving many small payments leaves you with many small coins. Spending them all at once
  makes a large transaction. Chapter 9 covers merging them.

## Amounts are whole numbers

One IMN is 100,000,000 **atoms**, and every amount in the protocol is a whole number of atoms.
Do your arithmetic in atoms and only convert to IMN for display. Floating-point numbers lose
atoms on amounts that look harmless, and a payment short by one atom is a payment that does
not match its invoice.

## Addresses and keys

A **private key** is 32 random bytes. From it comes a public key, and from that an **address**
such as `imntest:qqzh3r96a2v…`. The prefix names the network: `imntest:` on test networks,
`imn:` on a main network.

- Nothing is registered. An address exists the moment you have the key, even offline.
- A transaction that spends a coin carries a **signature** made with the key of the address
  the coin is locked to. Nodes check it; nobody else can produce it.
- The signature covers the whole transaction, including the network, so a signed payment
  cannot be altered or replayed on another network.

Whoever holds a private key controls its coins. There is no recovery and no support desk.

## Blocks, and why this is a DAG

Miners collect transactions into **blocks** about every five seconds. A block must carry
**proof of work**: evidence that the miner spent real computation, which is what makes
history expensive to rewrite.

In most chains each block has one parent, so when two miners find a block at the same moment
one is thrown away. Here a block can name **several parents**, so both are kept. The blocks
form a graph (a *DAG*) rather than a single line, and a rule called GHOSTDAG puts them in one
agreed order. Every node applies transactions in that order, and when two conflict, the
earlier one wins.

You rarely need to think about this. What matters in practice:

- A payment is normally in a block within about five seconds.
- Each new block built on top of it is one more **confirmation**.
- Blocks buried more than 12 hours deep are **final**: no node will replace them.

## How sure is "paid"?

A payment passes through three levels, and you choose how far to wait:

| Level | Meaning | Typical wait | Good for |
| :--- | :--- | :--- | :--- |
| `seen` | A node has the valid, signed payment and is relaying it | under a second | Small amounts, where a rare loss is acceptable |
| `included` | It is in a block | about 5 seconds | Everyday purchases |
| `final` | It has 60 confirmations and the network looks settled | about 5 minutes | Large amounts, anything irreversible |

A `seen` payment can still be replaced by its sender with a conflicting one. An `included`
payment can only disappear if the recent blocks are reorganised, which gets rapidly less
likely with each confirmation. The 60 is a default; an operator sets it to match what they
are protecting.

A mining reward is the one exception: it cannot be spent until 20 confirmations have passed.

## Invoices

Many payments may arrive at one address. To tell them apart, the payer's wallet signs an
**invoice ID** into the payment: 1 to 64 characters from `A-Z a-z 0-9 - _ .`, chosen by
whoever is being paid. A node indexes it, so "has order 1001 been paid, and how surely?" is a
single lookup. Because the ID is covered by the signature, nobody can attach it to a payment
afterwards or strip it off.

This is the feature the rest of the system is built around. Chapter 5 uses it.

Next: [a private network on your machine](02-private-network.md).
