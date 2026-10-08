#!/usr/bin/env bash
# Internet Money (IMN) All-in-One Node Daemon & GUI Wallet Launcher

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
echo "============================================================"
echo "  Internet Money (IMN) Testnet-1 All-in-One Node & Wallet"
echo "  Consensus: GHOSTDAG @ 5s Block Interval (0.2 BPS)"
echo "  Proof of Work: Hallmark (GPU/CPU Memory-Hard)"
echo "  GUI Wallet & Dashboard: http://127.0.0.1:18556/wallet"
echo "============================================================"

# Open wallet URL in default browser
if command -v xdg-open > /dev/null; then
  xdg-open "http://127.0.0.1:18556/wallet" > /dev/null 2>&1 &
elif command -v open > /dev/null; then
  open "http://127.0.0.1:18556/wallet" > /dev/null 2>&1 &
fi

exec "$DIR/target/release/imoney-node" --rpc-bind 127.0.0.1:18556 --auto-mine
