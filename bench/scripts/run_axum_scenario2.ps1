# Axum Scenario 2: Complex Auth Benchmark Runner

Write-Host "Building Axum Scenario 2 Server..." -ForegroundColor Cyan
cargo build --release --bin axum_scenario2

Write-Host "Generating Token..." -ForegroundColor Cyan
$token = &(cargo run --release --bin axum_scenario2 -- gen-token)

Write-Host "Starting Axum Scenario 2 Server..." -ForegroundColor Cyan
$serverProcess = Start-Process cargo -ArgumentList "run --release --bin axum_scenario2" -PassThru -NoNewWindow

Write-Host "Waiting for server to warm up (5s)..."
Start-Sleep -Seconds 5

Write-Host "Running wrk benchmark..." -ForegroundColor Yellow
# Using port 4001 for Axum comparison
# wrk -t12 -c400 -d30s -H "Authorization: Bearer $token" http://127.0.0.1:4001/protected

Write-Host "Stopping Axum Scenario 2 Server..." -ForegroundColor Cyan
$serverProcess.Kill()
