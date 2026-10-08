# Contributing

Thanks for looking. The project is small and pre-launch, so the process is light.

## Before you start

- For anything larger than a fix, open an issue first and say what you want to change. Changes
  to consensus rules, the transaction format or the proof of work reset the test network and
  need agreement before code.
- Security problems go through [SECURITY.md](SECURITY.md), not public issues.
- [ROADMAP.md](ROADMAP.md) lists what is missing.

## Building and testing

```bash
cargo build --release
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The Rust version is pinned in `rust-toolchain.toml`. CI runs the same three commands on Linux,
Windows and macOS, and a pull request must pass them.

The SDK:

```bash
cd packages/imoney-sdk && npm ci && npm test
```

`npm test` rebuilds the bundles in `dist/` and the copy in the WooCommerce plugin. Commit them
with your change; CI fails if they are out of date. The same goes for the wallet's signing
module in `apps/imoney-wallet/pkg` when you change `crates/imoney-wasm` (see the README).

A private network for trying changes, with its own genesis block:

```bash
cargo run --release --bin imoney-node -- --devnet mytest --auto-mine
```

## What a good change looks like

- It comes with a test that fails without it.
- It keeps claims honest. Documentation says what the software does today, with measured
  numbers where there are numbers.
- It does not add a dependency without a reason.
- It keeps other people's code separate and credited. `crates/imoney-pow/src/fishhash.rs` is a
  port under the MPL-2.0 and stays that way. Do not paste code whose licence you have not
  checked.

## Licence

Contributions are accepted under the same terms as the project: MIT or Apache-2.0 at the user's
option (MPL-2.0 for changes to `fishhash.rs`). The name and logo are covered by
[TRADEMARKS.md](TRADEMARKS.md).
