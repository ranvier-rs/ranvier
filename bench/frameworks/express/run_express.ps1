# run_express.ps1
param (
    [string]$Scenario = "1"
)

$port = 3000 + ([int]$Scenario - 1)
$env:SCENARIO = $Scenario

Write-Host "Starting Express Scenario $Scenario on port $port..."
Start-Process "node" -ArgumentList "index.js" -NoNewWindow

Start-Sleep -Seconds 2

# Check if wrk is available
if (Get-Command "wrk" -ErrorAction SilentlyContinue) {
    Write-Host "Running wrk benchmark..."
    if ($Scenario -eq "2") {
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

Write-Host "Benchmark complete. Please terminate the node process manually."
