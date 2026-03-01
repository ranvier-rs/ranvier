# run_fastapi.ps1
param (
    [string]$Scenario = "1"
)

# Start Uvicorn for the specified scenario in the background
$appName = "app" + $Scenario
$port = 3000 + ([int]$Scenario - 1)

Write-Host "Starting FastAPI Scenario $Scenario on port $port..."
Start-Process "uvicorn" -ArgumentList "main:$appName", "--host", "0.0.0.0", "--port", "$port", "--workers", "1", "--log-level", "warning" -NoNewWindow

Start-Sleep -Seconds 2

# Check if wrk is available
if (Get-Command "wrk" -ErrorAction SilentlyContinue) {
    Write-Host "Running wrk benchmark..."
    if ($Scenario -eq "2") {
        # Need to generate token or just pass a known good one.
        # Here we'll just show the command
        Write-Host "Please pass an auth token for Scenario 2."
        wrk -t4 -c100 -d10s -H "Authorization: Bearer <token>" "http://127.0.0.1:$port/protected"
    } elseif ($Scenario -eq "1") {
        wrk -t4 -c100 -d10s "http://127.0.0.1:$port/"
    } elseif ($Scenario -eq "3") {
        wrk -t4 -c100 -d10s "http://127.0.0.1:$port/workflow"
    } elseif ($Scenario -eq "4") {
        wrk -t4 -c100 -d10s "http://127.0.0.1:$port/concurrency"
    }
} else {
    Write-Host "`wrk` is not installed or not in PATH."
}

Write-Host "Benchmark complete. Please terminate the uvicorn process manually."
