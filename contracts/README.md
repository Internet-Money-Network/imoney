# The Ethereum side of an atomic swap

`src/ImnSwap.sol` is a hash lock for Ethereum and chains like it (Base, and the rest). Ether
or tokens locked in it go to the recipient when anyone presents the 32-byte secret whose
SHA-256 hash the lock names, or back to the sender after a deadline. An Internet Money
hash-lock address ([SPECIFICATION.md §8.4](../SPECIFICATION.md)) works the same way with the
same hash, so two people can trade IMN for ether or a token with no exchange and no custodian.

The contract has no owner, no fee, no upgrade path and no pause.

**It has not been audited and has not been deployed anywhere.** Do not use it with anything
of value yet.

## Build and test

    npm install
    npm test

The tests run on an in-process Ethereum chain and cover claims, refunds, deadlines, tokens
(including one that returns `false` instead of reverting), duplicate locks and a recipient
that tries to be paid twice.

## A swap, step by step

Alice has IMN and wants ether; Bob has ether and wants IMN.

1. Alice picks a random 32-byte secret and gives Bob its SHA-256 hash.
2. Alice locks her IMN for Bob under that hash, refundable after, say, 24 hours.
3. Bob checks Alice's lock on the Internet Money chain, then calls `lock` here for Alice
   under the same hash, refundable after 12 hours.
4. Alice calls `claim` here with the secret. She has her ether, and the secret is now public
   in the `Claimed` event.
5. Bob uses the secret to claim the IMN.

If Bob never locks, Alice takes her IMN back after 24 hours. If Alice never claims, Bob takes
his ether back after 12, and Alice her IMN after 24.

**The deadlines must be in that order.** Whoever knows the secret (Alice) claims second-locked
funds first, so the other side (Bob's ether) must have the *earlier* deadline, with hours to
spare. Otherwise Alice could wait, take her refund, and still claim.

Internet Money counts its deadline in blue score, about five seconds each; this contract
counts in Unix time. Convert generously.
