# Scenario 3: Multi-step Workflow Benchmark Runner

$server_bin = "scenario2_server" # Placeholder, will be scenario3_server
# Wait, I should use scenario3_server

Write-Host "Building Scenario 3 Server..." -ForegroundColor Cyan
cargo build --release --bin scenario3_server

Write-Host "Starting Scenario 3 Server..." -ForegroundColor Cyan
$serverProcess = Start-Process cargo -ArgumentList "run --release --bin scenario3_server" -PassThru -NoNewWindow

Write-Host "Waiting for server to warm up (5s)..."
Start-Sleep -Seconds 5

Write-Host "Running wrk benchmark..." -ForegroundColor Yellow
# Scenario 3 uses port 3002
wrk -t12 -c400 -d30s http://127.0.0.1:3002/workflow

Write-Host "Stopping Scenario 3 Server..." -ForegroundColor Cyan
$serverProcess.Kill()
