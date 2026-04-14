param(
    [int]$Connections = 100,
    [int]$DurationSeconds = 15,
    [int]$Pipelining = 10,
    [int]$StartupTimeoutSeconds = 20,
    [string]$Username = "admin",
    [string]$Password = "admin123"
)

$ErrorActionPreference = "Stop"
$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
. (Join-Path $ScriptRoot "benchmark_common.ps1")

$RanvierRoot = Split-Path -Parent $ScriptRoot
$WorkspaceRoot = Split-Path -Parent $RanvierRoot
$ServerExe = Join-Path $RanvierRoot "target\release\admin-crud-demo.exe"
$EvidencePath = New-EvidencePath -WorkspaceRoot $WorkspaceRoot -Prefix "m380_admin_crud_users_benchmark"
$BaseUrl = "http://127.0.0.1:3120"
$serverProcess = $null
$previousSuppress = $env:RANVIER_SUPPRESS_INCOMPLETE_MESSAGE_ERROR

Assert-CommandExists -Name "cargo" -InstallHint "Install Rust and ensure cargo is on PATH."
Assert-CommandExists -Name "npx" -InstallHint "Install Node.js so npx can run autocannon."

Push-Location $RanvierRoot
try {
    Write-Host "Building admin-crud-demo in release mode..."
    cargo build --release -p admin-crud-demo
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed for admin-crud-demo."
    }

    if (-not (Test-Path $ServerExe)) {
        throw "Expected benchmark binary not found: $ServerExe"
    }

    Write-Host "Starting admin-crud-demo..."
    $env:RANVIER_SUPPRESS_INCOMPLETE_MESSAGE_ERROR = "1"
    $serverProcess = Start-Process -FilePath $ServerExe -PassThru -NoNewWindow
    Wait-TcpPort -Address "127.0.0.1" -Port 3120 -TimeoutSeconds $StartupTimeoutSeconds

    Write-Host "Requesting JWT token from admin-crud-demo..."
    $loginBody = @{
        username = $Username
        password = $Password
    } | ConvertTo-Json

    $loginResponse = Invoke-RestMethod -Method Post -Uri "$BaseUrl/login" -ContentType "application/json" -Body $loginBody
    if (-not $loginResponse.token) {
        throw "Login response did not include a token."
    }

    Write-Host "Running realism benchmark on $BaseUrl/users"
    $npxPath = (Get-Command "npx.cmd").Source
    $argumentString = "-y autocannon --json -c $Connections -d $DurationSeconds -p $Pipelining -H `"authorization=Bearer $($loginResponse.token)`" $BaseUrl/users"
    $exitCode = Invoke-LoggedProcess -FilePath $npxPath -ArgumentList @($argumentString) -OutputPath $EvidencePath
    if ($exitCode -ne 0) {
        throw "autocannon benchmark failed for admin-crud-demo."
    }
    Assert-AutocannonHealthy -Path $EvidencePath -Target "admin-crud-demo GET /users"

    $memoryMb = Get-ProcessMemoryMb -ProcessId $serverProcess.Id
    Write-BenchmarkMetadata -Path $EvidencePath -Lines @(
        "Benchmark: M380 secondary realism baseline",
        "Target: admin-crud-demo GET /users",
        "Connections: $Connections",
        "DurationSeconds: $DurationSeconds",
        "Pipelining: $Pipelining",
        "MemoryMb: $memoryMb"
    )

    Write-Host "Saved admin-crud-demo benchmark evidence to $EvidencePath"
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
