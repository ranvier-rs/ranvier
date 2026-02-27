# Axum Scenario 1: Simple CRUD Benchmark Runner

Write-Host "Building Axum Scenario 1 Server..." -ForegroundColor Cyan
cargo build --release --bin axum_scenario1

Write-Host "Starting Axum Scenario 1 Server..." -ForegroundColor Cyan
$serverProcess = Start-Process cargo -ArgumentList "run --release --bin axum_scenario1" -PassThru -NoNewWindow

Write-Host "Waiting for server to warm up (5s)..."
Start-Sleep -Seconds 5

Write-Host "Running wrk benchmark..." -ForegroundColor Yellow
# Using port 4000 for Axum comparison
# wrk -t12 -c400 -d30s http://127.0.0.1:4000/
# Note: if wrk is missing, this will fail.

Write-Host "Stopping Axum Scenario 1 Server..." -ForegroundColor Cyan
$serverProcess.Kill()
