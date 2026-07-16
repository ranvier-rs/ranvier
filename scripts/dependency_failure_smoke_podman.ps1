param(
    [string]$PostgresContainer = "ranvier-rq8-postgres",
    [string]$RedisContainer = "ranvier-rq8-redis",
    [string]$PostgresImage = "postgres:16-alpine",
    [string]$RedisImage = "redis:7-alpine",
    [int]$PostgresPort = 54329,
    [int]$RedisPort = 6389,
    [string]$SummaryPath = ""
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

function Stop-ContainerIfExists {
    param([string]$Name)

    $existing = podman ps -a --format "{{.Names}}" | Where-Object { $_ -eq $Name }
    if ($existing) {
        podman rm -f $Name | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to remove existing container: $Name"
        }
    }
}

function Wait-PostgresReady {
    param([string]$ContainerName, [int]$MaxAttempts = 20)

    for ($attempt = 1; $attempt -le $MaxAttempts; $attempt++) {
        podman exec $ContainerName pg_isready -U ranvier -d ranvier | Out-Null
        if ($LASTEXITCODE -eq 0) {
            return $attempt
        }
        Start-Sleep -Seconds 1
    }
    throw "PostgreSQL readiness check failed for container: $ContainerName"
}

function Wait-RedisReady {
    param([string]$ContainerName, [int]$MaxAttempts = 20)

    for ($attempt = 1; $attempt -le $MaxAttempts; $attempt++) {
        $pong = podman exec $ContainerName redis-cli ping 2>$null
        if ($LASTEXITCODE -eq 0 -and $pong -match "PONG") {
            return $attempt
        }
        Start-Sleep -Seconds 1
    }
    throw "Redis readiness check failed for container: $ContainerName"
}

function Wait-ProbeMarker {
    param(
        [System.Diagnostics.Process]$Process,
        [string]$Path,
        [string]$Phase,
        [int]$MaxSeconds = 60
    )

    for ($attempt = 1; $attempt -le $MaxSeconds; $attempt++) {
        if (Test-Path -LiteralPath $Path -PathType Leaf) {
            return $attempt
        }
        if ($Process.HasExited) {
            throw "Probe exited before $Phase marker (exit $($Process.ExitCode))."
        }
        Start-Sleep -Seconds 1
    }
    throw "Timed out waiting for $Phase marker: $Path"
}

function Wait-ProbeExit {
    param(
        [System.Diagnostics.Process]$Process,
        [string]$Phase,
        [int]$MaxSeconds = 60
    )

    if (-not $Process.WaitForExit($MaxSeconds * 1000)) {
        $Process.Kill($true)
        throw "Timed out waiting for $Phase probe exit."
    }
    if ($Process.ExitCode -ne 0) {
        throw "$Phase probe failed with exit code $($Process.ExitCode)."
    }
}

function Start-Probe {
    param(
        [string]$Executable,
        [string]$Mode,
        [string]$ControlDirectory,
        [string]$RunId,
        [string]$StdoutPath,
        [string]$StderrPath
    )

    $quotedControlDirectory = '"' + $ControlDirectory.Replace('"', '\"') + '"'
    Start-Process -FilePath $Executable `
        -ArgumentList @($Mode, $quotedControlDirectory, $RunId) `
        -RedirectStandardOutput $StdoutPath `
        -RedirectStandardError $StderrPath `
        -WindowStyle Hidden `
        -PassThru
}

function Invoke-CargoGate {
    param([string[]]$Arguments, [string]$LogPath, [string]$Label)

    & cargo @Arguments *>&1 | Tee-Object -FilePath $LogPath | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "$Label failed. See $LogPath"
    }
}

if (-not (Get-Command podman -ErrorAction SilentlyContinue)) {
    throw "podman is not installed or not in PATH."
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo is not installed or not in PATH."
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ranvierRoot = (Resolve-Path (Join-Path $scriptDir "..")).Path
$workspaceRoot = (Resolve-Path (Join-Path $ranvierRoot "..")).Path
$evidenceDir = Join-Path $workspaceRoot "docs/05_dev_plans/evidence"
New-Item -ItemType Directory -Path $evidenceDir -Force | Out-Null

if ([string]::IsNullOrWhiteSpace($SummaryPath)) {
    $SummaryPath = Join-Path $evidenceDir "m419_rq8_dependency_failure_20260716.md"
}

$runId = Get-Date -Format "yyyyMMdd_HHmmss"
$controlRoot = Join-Path $ranvierRoot "target/rq8-dependency-$runId"
$postgresControl = Join-Path $controlRoot "postgres"
$redisControl = Join-Path $controlRoot "redis"
New-Item -ItemType Directory -Path $postgresControl, $redisControl -Force | Out-Null

$probeName = if ($IsWindows -or $env:OS -eq "Windows_NT") {
    "dependency_recovery_probe.exe"
} else {
    "dependency_recovery_probe"
}
$probe = Join-Path $ranvierRoot "target/debug/examples/$probeName"
$runtimeLog = Join-Path $controlRoot "runtime-persistence-tests.log"
$guardLog = Join-Path $controlRoot "guard-distributed-tests.log"
$postgresOut = Join-Path $controlRoot "postgres-live.stdout.log"
$postgresErr = Join-Path $controlRoot "postgres-live.stderr.log"
$redisOut = Join-Path $controlRoot "redis-live.stdout.log"
$redisErr = Join-Path $controlRoot "redis-live.stderr.log"
$crashWriteLog = Join-Path $controlRoot "postgres-crash-write.log"
$crashRecoverLog = Join-Path $controlRoot "postgres-crash-recover.log"

$postgresProbe = $null
$redisProbe = $null
$startedAt = Get-Date

try {
    Stop-ContainerIfExists -Name $PostgresContainer
    Stop-ContainerIfExists -Name $RedisContainer

    podman run -d --name $PostgresContainer `
        -e POSTGRES_USER=ranvier `
        -e POSTGRES_PASSWORD=ranvier `
        -e POSTGRES_DB=ranvier `
        -p "${PostgresPort}:5432" `
        $PostgresImage | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to start PostgreSQL fixture."
    }

    podman run -d --name $RedisContainer `
        -p "${RedisPort}:6379" `
        $RedisImage | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to start Redis fixture."
    }

    $postgresInitialReady = Wait-PostgresReady -ContainerName $PostgresContainer
    $redisInitialReady = Wait-RedisReady -ContainerName $RedisContainer

    $env:RANVIER_PERSISTENCE_POSTGRES_URL = "postgres://ranvier:ranvier@127.0.0.1:${PostgresPort}/ranvier"
    $env:RANVIER_PERSISTENCE_REDIS_URL = "redis://127.0.0.1:${RedisPort}"
    $env:REDIS_URL = $env:RANVIER_PERSISTENCE_REDIS_URL

    Push-Location $ranvierRoot
    try {
        Invoke-CargoGate `
            -Arguments @("build", "-p", "ranvier-runtime", "--example", "dependency_recovery_probe", "--features", "persistence-postgres,persistence-redis", "--locked") `
            -LogPath (Join-Path $controlRoot "probe-build.log") `
            -Label "dependency recovery probe build"

        Invoke-CargoGate `
            -Arguments @("test", "-p", "ranvier-runtime", "--features", "persistence-postgres,persistence-redis", "persistence", "--locked") `
            -LogPath $runtimeLog `
            -Label "configured persistence tests"

        Invoke-CargoGate `
            -Arguments @("test", "-p", "ranvier-guard", "--features", "distributed", "distributed", "--locked") `
            -LogPath $guardLog `
            -Label "distributed Guard tests"
    } finally {
        Pop-Location
    }

    if (-not (Test-Path -LiteralPath $probe -PathType Leaf)) {
        throw "Built probe executable is missing: $probe"
    }

    $postgresProbe = Start-Probe -Executable $probe -Mode "postgres-live" `
        -ControlDirectory $postgresControl -RunId $runId `
        -StdoutPath $postgresOut -StderrPath $postgresErr
    $postgresReadyMarker = Wait-ProbeMarker -Process $postgresProbe `
        -Path (Join-Path $postgresControl "ready") -Phase "PostgreSQL ready"
    podman stop --time 0 $PostgresContainer | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to stop PostgreSQL fixture for outage injection."
    }
    New-Item -ItemType File -Path (Join-Path $postgresControl "inject-outage") -Force | Out-Null
    $postgresOutageMarker = Wait-ProbeMarker -Process $postgresProbe `
        -Path (Join-Path $postgresControl "outage-observed") -Phase "PostgreSQL outage"
    podman start $PostgresContainer | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to restart PostgreSQL fixture."
    }
    $postgresRestartReady = Wait-PostgresReady -ContainerName $PostgresContainer
    New-Item -ItemType File -Path (Join-Path $postgresControl "inject-recovery") -Force | Out-Null
    $postgresRecoveryMarker = Wait-ProbeMarker -Process $postgresProbe `
        -Path (Join-Path $postgresControl "recovered") -Phase "PostgreSQL recovery"
    Wait-ProbeExit -Process $postgresProbe -Phase "PostgreSQL live"
    $postgresProbe = $null

    $redisProbe = Start-Probe -Executable $probe -Mode "redis-live" `
        -ControlDirectory $redisControl -RunId $runId `
        -StdoutPath $redisOut -StderrPath $redisErr
    $redisReadyMarker = Wait-ProbeMarker -Process $redisProbe `
        -Path (Join-Path $redisControl "ready") -Phase "Redis ready"
    podman stop --time 0 $RedisContainer | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to stop Redis fixture for outage injection."
    }
    New-Item -ItemType File -Path (Join-Path $redisControl "inject-outage") -Force | Out-Null
    $redisOutageMarker = Wait-ProbeMarker -Process $redisProbe `
        -Path (Join-Path $redisControl "outage-observed") -Phase "Redis outage"
    podman start $RedisContainer | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to restart Redis fixture."
    }
    $redisRestartReady = Wait-RedisReady -ContainerName $RedisContainer
    New-Item -ItemType File -Path (Join-Path $redisControl "inject-recovery") -Force | Out-Null
    $redisRecoveryMarker = Wait-ProbeMarker -Process $redisProbe `
        -Path (Join-Path $redisControl "recovered") -Phase "Redis recovery"
    Wait-ProbeExit -Process $redisProbe -Phase "Redis live"
    $redisProbe = $null

    & $probe "postgres-crash-write" $runId *>&1 | Tee-Object -FilePath $crashWriteLog | Out-Null
    $crashExit = $LASTEXITCODE
    if ($crashExit -ne 86) {
        throw "Expected crash-boundary exit 86, got $crashExit."
    }

    & $probe "postgres-crash-recover" $runId *>&1 | Tee-Object -FilePath $crashRecoverLog | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "PostgreSQL process-recovery phase failed."
    }

    $requiredMarkers = @(
        @{ Path = $postgresOut; Pattern = "POSTGRES_OUTAGE_" },
        @{ Path = $postgresOut; Pattern = "POSTGRES_SAME_INSTANCE_RECOVERED" },
        @{ Path = $redisOut; Pattern = "REDIS_OUTAGE_" },
        @{ Path = $redisOut; Pattern = "REDIS_SAME_MANAGER_RECOVERED" },
        @{ Path = $crashWriteLog; Pattern = "POSTGRES_CRASH_BOUNDARY_COMMITTED" },
        @{ Path = $crashRecoverLog; Pattern = "POSTGRES_PROCESS_RECOVERY_OK" },
        @{ Path = $crashRecoverLog; Pattern = "POSTGRES_COMPENSATION_MARKER_RECOVERED" },
        @{ Path = $crashRecoverLog; Pattern = "POSTGRES_COMPLETED_RESUME_REJECTED" }
    )
    foreach ($marker in $requiredMarkers) {
        if (-not (Select-String -Path $marker.Path -Pattern $marker.Pattern -SimpleMatch)) {
            throw "Missing required evidence marker '$($marker.Pattern)' in $($marker.Path)."
        }
    }

    $podmanVersion = (podman version --format "{{.Client.Version}}" 2>$null).Trim()
    if ([string]::IsNullOrWhiteSpace($podmanVersion)) {
        $podmanVersion = (podman version | Select-Object -First 1).Trim()
    }
    $postgresImageId = (podman image inspect $PostgresImage --format "{{.Id}}").Trim()
    $redisImageId = (podman image inspect $RedisImage --format "{{.Id}}").Trim()
    $rustcVersion = (& rustc --version).Trim()
    $cargoVersion = (& cargo --version).Trim()
    $completedAt = Get-Date
    $elapsedSeconds = [math]::Round(($completedAt - $startedAt).TotalSeconds, 1)

    $summary = @"
# M419-RQ8 Dependency Failure and Crash-Recovery Evidence

**Captured:** $($completedAt.ToString("yyyy-MM-ddTHH:mm:sszzz"))

**Result:** Pass

## Fixture

| Item | Value |
|---|---|
| Podman | $podmanVersion |
| Rust | $rustcVersion |
| Cargo | $cargoVersion |
| PostgreSQL image | $PostgresImage / $postgresImageId |
| Redis image | $RedisImage / $redisImageId |
| Loopback ports | PostgreSQL $PostgresPort; Redis $RedisPort |
| Total elapsed | $elapsedSeconds seconds |

## Live Results

| Scenario | Result | Bounded observation |
|---|---|---|
| PostgreSQL initial readiness | Pass | attempt $postgresInitialReady |
| PostgreSQL actual stop and outage | Pass | probe marker $postgresOutageMarker; operation errored or timed out within 5 seconds |
| PostgreSQL same-store recovery | Pass | container readiness $postgresRestartReady; probe marker $postgresRecoveryMarker; committed row preserved |
| Redis initial readiness | Pass | attempt $redisInitialReady |
| Redis actual stop and outage | Pass | probe marker $redisOutageMarker; operation errored or timed out within 5 seconds |
| Redis same-manager recovery | Pass | container readiness $redisRestartReady; probe marker $redisRecoveryMarker; new operation accepted |
| PostgreSQL process boundary | Pass | writer exited with declared code $crashExit; new process recovered next step 2 |
| compensation marker after restart | Pass | new process observed the committed PostgreSQL idempotency key |
| completed-trace resume | Pass | new process rejected ordinary resume |

The live PostgreSQL reconnect reused one `PostgresPersistenceStore` and pool.
The live Redis reconnect reused one `RedisPersistenceStore` and connection
manager. Redis data survival is deliberately not asserted. The compensation
marker proves duplicate suppression only after the marker is committed; the
external side-effect/marker crash window remains at-least-once.

## Deterministic Gates Run With the Fixture

- configured Runtime PostgreSQL/Redis persistence and compensation tests: pass;
- distributed Guard fail-open/fail-closed, bounded window, outage, and recovery
  tests with live Redis: pass.

RQ8's Inspector, Audit, HTTP lifecycle, panic/timeout, complete workspace, API,
and SemVer gates are recorded in the parent M419 evidence record rather than
being inferred from this external-dependency harness.

## Commands

- cargo build -p ranvier-runtime --example dependency_recovery_probe --features persistence-postgres,persistence-redis --locked
- cargo test -p ranvier-runtime --features persistence-postgres,persistence-redis persistence --locked
- cargo test -p ranvier-guard --features distributed distributed --locked
- dependency_recovery_probe postgres-live CONTROL_DIR RUN_ID, with an actual podman stop/start between its markers
- dependency_recovery_probe redis-live CONTROL_DIR RUN_ID, with an actual podman stop/start between its markers
- dependency_recovery_probe postgres-crash-write RUN_ID, followed by postgres-crash-recover in a new process
"@
    Set-Content -LiteralPath $SummaryPath -Value $summary -Encoding utf8

    Write-Host "M419-RQ8 dependency failure smoke passed."
    Write-Host "SUMMARY=$SummaryPath"
    Write-Host "LOCAL_LOG_DIR=$controlRoot"
} finally {
    if ($postgresProbe -and -not $postgresProbe.HasExited) {
        $postgresProbe.Kill($true)
    }
    if ($redisProbe -and -not $redisProbe.HasExited) {
        $redisProbe.Kill($true)
    }
    Stop-ContainerIfExists -Name $PostgresContainer
    Stop-ContainerIfExists -Name $RedisContainer
}
