# Requires Node.js to use npx autocannon
$ErrorActionPreference = "Stop"
$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
. (Join-Path $ScriptRoot "benchmark_common.ps1")

$RanvierRoot = Split-Path -Parent $ScriptRoot
$WorkspaceRoot = Split-Path -Parent $RanvierRoot
$ServerExe = Join-Path $RanvierRoot "target\release\hello-world.exe"
$outFilePath = New-EvidencePath -WorkspaceRoot $WorkspaceRoot -Prefix "m134_http_autocannon"
$serverProcess = $null

Assert-CommandExists -Name "cargo" -InstallHint "Install Rust and ensure cargo is on PATH."
Assert-CommandExists -Name "npx" -InstallHint "Install Node.js so npx can run autocannon."

Push-Location $RanvierRoot
try {
    Write-Host "Building hello-world example in release mode..."
    cargo build --release -p hello-world
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed for hello-world."
    }

    if (-not (Test-Path $ServerExe)) {
        throw "Expected benchmark binary not found: $ServerExe"
    }

    Write-Host "Starting hello-world server..."
    $serverProcess = Start-Process -FilePath $ServerExe -PassThru -NoNewWindow
    Wait-TcpPort -Address "127.0.0.1" -Port 3000 -TimeoutSeconds 20

    Write-Host "Running autocannon benchmark on http://127.0.0.1:3000/"
    $npxPath = (Get-Command "npx.cmd").Source
    $argumentString = "-y autocannon --json -c 100 -d 15 -p 10 http://127.0.0.1:3000/"
    $exitCode = Invoke-LoggedProcess -FilePath $npxPath -ArgumentList @($argumentString) -OutputPath $outFilePath
    if ($exitCode -ne 0) {
        throw "autocannon benchmark failed for hello-world."
    }
    Assert-AutocannonHealthy -Path $outFilePath -Target "hello-world /"

    Write-Host "Measuring Memory Usage..."
    $mem = Get-ProcessMemoryMb -ProcessId $serverProcess.Id
    Write-Host "Memory Footprint: $mem MB"
    Add-Content -Path $outFilePath -Value "`n---`nMemory Footprint: $mem MB"

    Write-Host "Benchmark finished. Results saved to $outFilePath"
}
finally {
    Pop-Location
    if ($serverProcess -and -not $serverProcess.HasExited) {
        Write-Host "Shutting down server..."
        Stop-Process -Id $serverProcess.Id -Force
    }
}

Write-Host "Done!"
