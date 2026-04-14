param(
    [int]$Connections = 100,
    [int]$DurationSeconds = 15,
    [int]$Pipelining = 10,
    [int]$StartupTimeoutSeconds = 20
)

$ErrorActionPreference = "Stop"
$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
. (Join-Path $ScriptRoot "benchmark_common.ps1")

$RanvierRoot = Split-Path -Parent $ScriptRoot
$WorkspaceRoot = Split-Path -Parent $RanvierRoot
$ServerExe = Join-Path $RanvierRoot "target\release\scenario2_server.exe"
$EvidencePath = New-EvidencePath -WorkspaceRoot $WorkspaceRoot -Prefix "m380_auth_request_context_benchmark"
$serverProcess = $null
$previousSuppress = $env:RANVIER_SUPPRESS_INCOMPLETE_MESSAGE_ERROR

Assert-CommandExists -Name "cargo" -InstallHint "Install Rust and ensure cargo is on PATH."
Assert-CommandExists -Name "npx" -InstallHint "Install Node.js so npx can run autocannon."

Push-Location $RanvierRoot
try {
    Write-Host "Building scenario2_server in release mode..."
    cargo build --release --bin scenario2_server
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed for scenario2_server."
    }

    if (-not (Test-Path $ServerExe)) {
        throw "Expected benchmark binary not found: $ServerExe"
    }

    Write-Host "Generating auth benchmark token..."
    $token = & $ServerExe gen-token
    if (-not $token) {
        throw "Failed to generate auth benchmark token."
    }

    Write-Host "Starting scenario2_server..."
    $env:RANVIER_SUPPRESS_INCOMPLETE_MESSAGE_ERROR = "1"
    $serverProcess = Start-Process -FilePath $ServerExe -PassThru -NoNewWindow
    Wait-TcpPort -Address "127.0.0.1" -Port 3001 -TimeoutSeconds $StartupTimeoutSeconds

    Write-Host "Running auth/request-context benchmark on http://127.0.0.1:3001/protected"
    $npxPath = (Get-Command "npx.cmd").Source
    $argumentString = "-y autocannon --json -c $Connections -d $DurationSeconds -p $Pipelining -H `"authorization=Bearer $token`" http://127.0.0.1:3001/protected"
    $exitCode = Invoke-LoggedProcess -FilePath $npxPath -ArgumentList @($argumentString) -OutputPath $EvidencePath
    if ($exitCode -ne 0) {
        throw "autocannon benchmark failed for scenario2_server."
    }
    Assert-AutocannonHealthy -Path $EvidencePath -Target "scenario2_server /protected"

    $memoryMb = Get-ProcessMemoryMb -ProcessId $serverProcess.Id
    Write-BenchmarkMetadata -Path $EvidencePath -Lines @(
        "Benchmark: M380 primary auth/request-context baseline",
        "Target: scenario2_server /protected",
        "Connections: $Connections",
        "DurationSeconds: $DurationSeconds",
        "Pipelining: $Pipelining",
        "MemoryMb: $memoryMb"
    )

    Write-Host "Saved auth/request-context benchmark evidence to $EvidencePath"
}
finally {
    Pop-Location
    if ($null -eq $previousSuppress) {
        Remove-Item Env:RANVIER_SUPPRESS_INCOMPLETE_MESSAGE_ERROR -ErrorAction SilentlyContinue
    } else {
        $env:RANVIER_SUPPRESS_INCOMPLETE_MESSAGE_ERROR = $previousSuppress
    }
    if ($serverProcess -and -not $serverProcess.HasExited) {
        Stop-Process -Id $serverProcess.Id -Force
    }
}
