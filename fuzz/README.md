# Fuzzing

Coverage-guided fuzzing of the code that reads bytes from strangers: block and transaction
decoding, signature-script checking, the peer-to-peer message decoder and address parsing.

Needs a nightly compiler and [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz), on Linux
or macOS:

```bash
cargo install cargo-fuzz
cargo +nightly fuzz run block_decode          # runs until stopped or a crash is found
cargo +nightly fuzz run transaction_verify -- -max_total_time=300
cargo +nightly fuzz run p2p_message
cargo +nightly fuzz run address_decode
```

A crash is saved under `fuzz/artifacts/<target>/`; `cargo +nightly fuzz run <target> <file>`
replays it. If it could be used against a node, report it as described in
[SECURITY.md](../SECURITY.md) rather than in a public issue.

CI runs each target for a short time on every push. That catches regressions, not deep bugs;
long runs on a dedicated machine are still to do.
