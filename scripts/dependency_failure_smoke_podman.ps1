param(
    [string]$PostgresContainer = ("ranvier-rq8-postgres-" + [Guid]::NewGuid().ToString("N")),
    [string]$RedisContainer = ("ranvier-rq8-redis-" + [Guid]::NewGuid().ToString("N")),
    [string]$PostgresImage = "docker.io/library/postgres:16-alpine",
    [string]$RedisImage = "docker.io/library/redis:7-alpine",
    [int]$PostgresPort = 54329,
    [int]$RedisPort = 6389,
    [string]$SummaryPath = ""
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

$ownedPostgres = $null
$ownedRedis = $null
$primaryFailure = $null
$savedEnvironment = @{}
foreach ($name in @('RANVIER_PERSISTENCE_POSTGRES_URL','RANVIER_PERSISTENCE_REDIS_URL','REDIS_URL')) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
}
function Assert-NewContainer {
    param([string]$Name)
    podman container exists $Name | Out-Null
    if ($LASTEXITCODE -eq 0) { throw 'Existing container preserved.' }
    if ($LASTEXITCODE -ne 1) { throw 'Container query failed; resources preserved.' }
}
function Get-OwnedContainer {
    param([object[]]$Output, [int]$Code)
    if ($Code -ne 0) { throw 'Container creation failed.' }
    $id = ($Output -join '').Trim()
    if ($id -notmatch '^[a-f0-9]{64}$') { throw 'Invalid owned container ID; resource preserved.' }
    return $id
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
$evidenceDir = Join-Path ([IO.Path]::GetTempPath()) ("ranvier-dependency-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $evidenceDir -Force | Out-Null

if ([string]::IsNullOrWhiteSpace($SummaryPath)) {
    $SummaryPath = Join-Path $evidenceDir "dependency_failure_summary.md"
}

if (Test-Path -LiteralPath $SummaryPath) { throw "Summary path already exists; prior evidence preserved." }
$runId = [Guid]::NewGuid().ToString("N")
$controlRoot = Join-Path $evidenceDir "control"
$postgresControl = Join-Path $controlRoot "postgres"
$redisControl = Join-Path $controlRoot "redis"
New-Item -ItemType Directory -Path $postgresControl, $redisControl -Force | Out-Null

$probeName = if ($IsWindows -or $env:OS -eq "Windows_NT") {
    "dependency_recovery_probe.exe"
} else {
    "dependency_recovery_probe"
}
$targetRoot = if ($env:CARGO_TARGET_DIR) { [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR) } else { Join-Path $ranvierRoot "target" }
$probe = Join-Path $targetRoot "debug/examples/$probeName"
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
    Assert-NewContainer $PostgresContainer
    Assert-NewContainer $RedisContainer

    $created = @(podman run -d --name $PostgresContainer `
        -e POSTGRES_USER=ranvier `
        -e POSTGRES_PASSWORD=ranvier `
        -e POSTGRES_DB=ranvier `
        -p "127.0.0.1:${PostgresPort}:5432" `
        $PostgresImage)
    $ownedPostgres = Get-OwnedContainer $created $LASTEXITCODE

    $created = @(podman run -d --name $RedisContainer `
        -p "127.0.0.1:${RedisPort}:6379" `
        $RedisImage)
    $ownedRedis = Get-OwnedContainer $created $LASTEXITCODE

    $postgresInitialReady = Wait-PostgresReady -ContainerName $ownedPostgres
    $redisInitialReady = Wait-RedisReady -ContainerName $ownedRedis

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
    podman stop --time 0 $ownedPostgres | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to stop PostgreSQL fixture for outage injection."
    }
    New-Item -ItemType File -Path (Join-Path $postgresControl "inject-outage") -Force | Out-Null
    $postgresOutageMarker = Wait-ProbeMarker -Process $postgresProbe `
        -Path (Join-Path $postgresControl "outage-observed") -Phase "PostgreSQL outage"
    podman start $ownedPostgres | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to restart PostgreSQL fixture."
    }
    $postgresRestartReady = Wait-PostgresReady -ContainerName $ownedPostgres
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
    podman stop --time 0 $ownedRedis | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to stop Redis fixture for outage injection."
    }
    New-Item -ItemType File -Path (Join-Path $redisControl "inject-outage") -Force | Out-Null
    $redisOutageMarker = Wait-ProbeMarker -Process $redisProbe `
        -Path (Join-Path $redisControl "outage-observed") -Phase "Redis outage"
    podman start $ownedRedis | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to restart Redis fixture."
    }
    $redisRestartReady = Wait-RedisReady -ContainerName $ownedRedis
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
    $postgresImageId = (podman container inspect $ownedPostgres --format '{{.Image}}' | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $postgresImageId -notmatch '^(sha256:)?[a-f0-9]{64}$') { throw 'PostgreSQL fixture image identity query failed.' }
    $redisImageId = (podman container inspect $ownedRedis --format '{{.Image}}' | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $redisImageId -notmatch '^(sha256:)?[a-f0-9]{64}$') { throw 'Redis fixture image identity query failed.' }
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
    # Publish the success summary only after all cleanup completes.

} catch {
    $primaryFailure = $_
    throw
} finally {
    foreach ($name in $savedEnvironment.Keys) {
        if ($null -eq $savedEnvironment[$name]) { Remove-Item "Env:$name" -ErrorAction SilentlyContinue }
        else { [Environment]::SetEnvironmentVariable($name,$savedEnvironment[$name]) }
    }
    $cleanupErrors = [Collections.Generic.List[string]]::new()
    foreach ($process in @($postgresProbe,$redisProbe)) {
        try { if ($process -and -not $process.HasExited) { $process.Kill($true); if (-not $process.WaitForExit(5000)) { throw "Owned probe did not terminate." } } }
        catch { $cleanupErrors.Add($_.Exception.Message) }
    }
    foreach ($id in @($ownedPostgres,$ownedRedis)) {
        if (-not $id) { continue }
        try { podman rm -f -v $id | Out-Null; if ($LASTEXITCODE -ne 0) { throw 'Owned container cleanup failed.' } }
        catch { $cleanupErrors.Add($_.Exception.Message) }
    }
    if ($cleanupErrors.Count) {
        if ($primaryFailure) { Write-Warning ($cleanupErrors -join '; ') }
        else { throw ($cleanupErrors -join '; ') }
    }
}
Set-Content -LiteralPath $SummaryPath -Value $summary -Encoding utf8
Write-Host 'Dependency failure smoke passed; owned resources removed.'
Write-Host "SUMMARY=$SummaryPath"
Write-Host "LOCAL_LOG_DIR=$controlRoot"