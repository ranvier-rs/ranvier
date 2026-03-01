# Scenario 2: Complex Auth Flow Benchmark Script (Ranvier)

Write-Host "--- Scenario 2: Complex Auth Flow (Ranvier) ---" -ForegroundColor Cyan

# 1. Build and Start Server in background
Write-Host "[1/4] Building Ranvier benchmark server..."
cargo build --release --bin scenario2_server

# 2. Generate a valid JWT token
Write-Host "[2/4] Generating benchmark token..."
$token = cargo run --release --bin scenario2_server -- gen-token
if (-not $token) {
    Write-Error "Failed to generate token"
    exit 1
}
Write-Host "Token generated."

# 3. Start Server
Write-Host "[3/4] Starting server..."
$serverProcess = Start-Process -FilePath "cargo" -ArgumentList "run", "--release", "--bin", "scenario2_server" -PassThru -NoNewWindow
Start-Sleep -Seconds 5

try {
    # 4. Execute Benchmark (wrk)
    # Note: Use the generated token in the Authorization header
    Write-Host "[4/4] Executing wrk benchmark (30s, 12 threads, 400 connections)..."
    wrk -t12 -c400 -d30s -H "Authorization: Bearer $token" http://127.0.0.1:3001/protected

    Write-Host "Shutting down server..."
}
finally {
    if ($serverProcess) {
        Stop-Process -Id $serverProcess.Id -Force
    }
}

Write-Host "Benchmark complete." -ForegroundColor Green
