@echo off
setlocal
title Internet Money (IMN) All-in-One Node & GUI Wallet
cd /d "%~dp0"
echo ============================================================
echo   Internet Money (IMN) Testnet Node & Wallet
echo   Consensus: GHOSTDAG @ 5s Block Interval (0.2 BPS)
echo   Proof of Work: Money Printer (GPU/CPU Memory-Hard)
echo   GUI Wallet & Dashboard: http://127.0.0.1:18556/wallet
echo ============================================================

start http://127.0.0.1:18556/wallet

rem Mining is done by START-TESTNET-MINER.bat (graphics card), not by the node
"%~dp0target\release\imoney-node.exe" --rpc-bind 127.0.0.1:18556
pause
