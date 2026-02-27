# Scenario 4: High Concurrency Benchmark Runner

Write-Host "Building Scenario 4 Server..." -ForegroundColor Cyan
cargo build --release --bin scenario4_server

Write-Host "Starting Scenario 4 Server..." -ForegroundColor Cyan
$serverProcess = Start-Process cargo -ArgumentList "run --release --bin scenario4_server" -PassThru -NoNewWindow

Write-Host "Waiting for server to warm up (5s)..."
Start-Sleep -Seconds 5

Write-Host "Running wrk benchmark (High Concurrency: 800 connections)..." -ForegroundColor Yellow
# Scenario 4 uses port 3003
wrk -t12 -c800 -d30s http://127.0.0.1:3003/concurrency

Write-Host "Stopping Scenario 4 Server..." -ForegroundColor Cyan
$serverProcess.Kill()
