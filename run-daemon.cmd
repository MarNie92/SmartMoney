@echo off
rem Starts the SmartMoney daemon (collector, scorer, paper trading,
rem dashboard at http://127.0.0.1:8080). Close the window to stop it.
cd /d "%~dp0"
set PATH=%USERPROFILE%\.cargo\bin;%PATH%
cargo run --release -p smartmoney-daemon
pause
