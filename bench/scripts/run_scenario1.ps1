# Scenario 1: Simple CRUD Benchmark Script (Ranvier)

Write-Host "--- Scenario 1: Simple CRUD (Ranvier) ---" -ForegroundColor Cyan

# 1. Build and Start Server in background
Write-Host "[1/3] Building and starting Ranvier benchmark server..."
$serverProcess = Start-Process -FilePath "cargo" -ArgumentList "run", "--release", "--bin", "scenario1_server" -PassThru -NoNewWindow
Start-Sleep -Seconds 5 # Wait for server to start

try {
    # 2. Execute Benchmark (wrk)
    # Adjust threads and connections as needed
    Write-Host "[2/3] Executing wrk benchmark (30s, 12 threads, 400 connections)..."
    wrk -t12 -c400 -d30s http://127.0.0.1:3000

    # 3. Teardown
    Write-Host "[3/3] Shutting down server..."
}
finally {
    if ($serverProcess) {
        Stop-Process -Id $serverProcess.Id -Force
    }
}

Write-Host "Benchmark complete." -ForegroundColor Green
