# Axum Scenario 4: High Concurrency Benchmark Runner

Write-Host "Building Axum Scenario 4 Server..." -ForegroundColor Cyan
cargo build --release --bin axum_scenario4

Write-Host "Starting Axum Scenario 4 Server..." -ForegroundColor Cyan
$serverProcess = Start-Process cargo -ArgumentList "run --release --bin axum_scenario4" -PassThru -NoNewWindow

Write-Host "Waiting for server to warm up (5s)..."
Start-Sleep -Seconds 5

Write-Host "Running wrk benchmark (High Concurrency: 800 connections)..." -ForegroundColor Yellow
# Using port 4003 for Axum comparison
# wrk -t12 -c800 -d30s http://127.0.0.1:4003/concurrency

Write-Host "Stopping Axum Scenario 4 Server..." -ForegroundColor Cyan
$serverProcess.Kill()
