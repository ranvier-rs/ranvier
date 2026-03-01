# Axum Scenario 3: Multi-step Workflow Benchmark Runner

Write-Host "Building Axum Scenario 3 Server..." -ForegroundColor Cyan
cargo build --release --bin axum_scenario3

Write-Host "Starting Axum Scenario 3 Server..." -ForegroundColor Cyan
$serverProcess = Start-Process cargo -ArgumentList "run --release --bin axum_scenario3" -PassThru -NoNewWindow

Write-Host "Waiting for server to warm up (5s)..."
Start-Sleep -Seconds 5

Write-Host "Running wrk benchmark..." -ForegroundColor Yellow
# Using port 4002 for Axum comparison
# wrk -t12 -c400 -d30s http://127.0.0.1:4002/workflow

Write-Host "Stopping Axum Scenario 3 Server..." -ForegroundColor Cyan
$serverProcess.Kill()
