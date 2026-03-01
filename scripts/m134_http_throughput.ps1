# Requires Node.js to use npx autocannon
$ErrorActionPreference = "Stop"

Write-Host "Building hello-world example in release mode..."
cargo build --release -p hello-world

Write-Host "Starting hello-world server..."
$serverProcess = Start-Process -FilePath ".\target\release\hello-world.exe" -PassThru -NoNewWindow

Write-Host "Waiting for server to start..."
Start-Sleep -Seconds 3

Write-Host "Running autocannon benchmark on http://127.0.0.1:3000/"
$outFilePath = "..\docs\05_dev_plans\evidence\m134_http_autocannon.txt"
npx -y autocannon -c 100 -d 15 -p 10 "http://127.0.0.1:3000/" | Out-File -FilePath $outFilePath -Encoding UTF8

Write-Host "Measuring Memory Usage..."
$mem = [math]::Round(((Get-Process -Id $serverProcess.Id).WorkingSet64 / 1MB), 2)
Write-Host "Memory Footprint: $mem MB"
Add-Content -Path $outFilePath -Value "`n---`nMemory Footprint: $mem MB"

Write-Host "Benchmark finished. Results saved to $outFilePath"
Write-Host "Shutting down server..."
Stop-Process -Id $serverProcess.Id -Force

Write-Host "Done!"
