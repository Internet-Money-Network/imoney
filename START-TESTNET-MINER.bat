@echo off
setlocal
title Internet Money (IMN) Miner
cd /d "%~dp0"
echo ============================================================
echo   Internet Money (IMN) - Money Printer PoW Miner
echo ============================================================
echo   Mining for the node at http://127.0.0.1:18556 (start the node first)
"%~dp0target\release\imoney-gpu-miner.exe" --node http://127.0.0.1:18556
pause
