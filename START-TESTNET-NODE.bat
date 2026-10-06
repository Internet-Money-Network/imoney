@echo off
setlocal
title Internet Money (IMN) Testnet Node
cd /d "%~dp0"
echo ============================================================
echo   Internet Money (IMN) Testnet-1 Node
echo   Consensus: GHOSTDAG @ 5s Block Interval (0.2 BPS)
echo   Web Dashboard & RPC: http://127.0.0.1:18556
echo ============================================================
"%~dp0target\release\imoney-node.exe" --rpc-bind 127.0.0.1:18556 --auto-mine
pause
