@echo off
setlocal
title Internet Money (IMN) Miner
cd /d "%~dp0"
echo ============================================================
echo   Internet Money (IMN) - Money Printer PoW Miner
echo ============================================================
"%~dp0target\release\imoney-miner.exe" --mine-test
pause
