param(
    [string]$PostgresContainer = "",
    [string]$RedisContainer = "",
    [ValidateRange(1, 65535)][int]$PostgresPort = 54329,
    [ValidateRange(1, 65535)][int]$RedisPort = 6389,
    [ValidateRange(0, 60)][int]$StartupWaitSeconds = 6,
    [string]$EvidenceDir = ""
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

function Assert-ContainerAbsent {
    param([string]$Name)
    podman container exists $Name | Out-Null
    $queryExit = $LASTEXITCODE
    if ($queryExit -eq 0) { throw "Container already exists; choose a fresh name: $Name" }
    if ($queryExit -ne 1) { throw "Container existence query failed ($queryExit): $Name" }
}

function Read-CreatedContainerId {
    param([object[]]$Output, [int]$ExitCode)
    if ($ExitCode -ne 0) { throw "podman run failed ($ExitCode). Existing resources are preserved." }
    $id = ($Output -join "`n").Trim()
    if ($id -notmatch '^[a-f0-9]{64}$') { throw "podman run did not return a valid owned container ID; resource preserved." }
    return $id
}

function Wait-PostgresReady {
    param(
        [string]$ContainerName,
        [int]$MaxAttempts = 20
    )

    for ($i = 0; $i -lt $MaxAttempts; $i++) {
        podman exec $ContainerName pg_isready -U ranvier -d ranvier | Out-Null
        if ($LASTEXITCODE -eq 0) {
            return
        }
        Start-Sleep -Seconds 1
    }
    throw "PostgreSQL readiness check failed for container: $ContainerName"
}

function Wait-RedisReady {
    param(
        [string]$ContainerName,
        [int]$MaxAttempts = 20
    )

    for ($i = 0; $i -lt $MaxAttempts; $i++) {
        $pong = podman exec $ContainerName redis-cli ping 2>$null
        if ($LASTEXITCODE -eq 0 -and $pong -match "PONG") {
            return
        }
        Start-Sleep -Seconds 1
    }
    throw "Redis readiness check failed for container: $ContainerName"
}

if (-not (Get-Command podman -ErrorAction SilentlyContinue)) {
    throw "podman is not installed or not in PATH."
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ranvierRoot = (Resolve-Path (Join-Path $scriptDir "..")).Path
$workspaceRoot = (Resolve-Path (Join-Path $ranvierRoot "..")).Path
$runId = [Guid]::NewGuid().ToString("N")
if ([string]::IsNullOrWhiteSpace($PostgresContainer)) { $PostgresContainer = "ranvier-persistence-pg-$runId" }
if ([string]::IsNullOrWhiteSpace($RedisContainer)) { $RedisContainer = "ranvier-persistence-redis-$runId" }
if ($PostgresContainer -eq $RedisContainer) { throw "PostgreSQL and Redis require distinct container names." }
if ([string]::IsNullOrWhiteSpace($EvidenceDir)) {
    $EvidenceDir = Join-Path ([System.IO.Path]::GetTempPath()) "ranvier-persistence-$runId"
}
$evidenceDir = [System.IO.Path]::GetFullPath($EvidenceDir)
$postgresOwnedId = $null
$redisOwnedId = $null
$primaryFailure = $null
$originalPostgresUrl = [Environment]::GetEnvironmentVariable("RANVIER_PERSISTENCE_POSTGRES_URL")
$originalRedisUrl = [Environment]::GetEnvironmentVariable("RANVIER_PERSISTENCE_REDIS_URL")
New-Item -ItemType Directory -Path $evidenceDir -Force | Out-Null

$stamp = Get-Date -Format "yyyyMMdd_HHmmss"
$testLog = Join-Path $evidenceDir ("persistence_adapter_smoke_test_" + $stamp + ".log")
$postgresLog = Join-Path $evidenceDir ("persistence_adapter_smoke_postgres_" + $stamp + ".log")
$redisLog = Join-Path $evidenceDir ("persistence_adapter_smoke_redis_" + $stamp + ".log")

try {
    Assert-ContainerAbsent -Name $PostgresContainer
    Assert-ContainerAbsent -Name $RedisContainer

    $created = @(podman run -d --rm --name $PostgresContainer `
        -e POSTGRES_USER=ranvier `
        -e POSTGRES_PASSWORD=ranvier `
        -e POSTGRES_DB=ranvier `
        -p "127.0.0.1:${PostgresPort}:5432" `
        postgres:16-alpine)
    $postgresOwnedId = Read-CreatedContainerId -Output $created -ExitCode $LASTEXITCODE

    $created = @(podman run -d --rm --name $RedisContainer `
        -p "127.0.0.1:${RedisPort}:6379" `
        redis:7-alpine)
    $redisOwnedId = Read-CreatedContainerId -Output $created -ExitCode $LASTEXITCODE

    Start-Sleep -Seconds $StartupWaitSeconds
    Wait-PostgresReady -ContainerName $postgresOwnedId
    Wait-RedisReady -ContainerName $redisOwnedId

    $env:RANVIER_PERSISTENCE_POSTGRES_URL = "postgres://ranvier:ranvier@127.0.0.1:${PostgresPort}/ranvier"
    $env:RANVIER_PERSISTENCE_REDIS_URL = "redis://127.0.0.1:${RedisPort}"

    Push-Location $ranvierRoot
    try {
        cargo test --locked -p ranvier-runtime --features persistence-postgres,persistence-redis *>&1 | Tee-Object -FilePath $testLog | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "cargo test failed for persistence adapter smoke."
        }
    } finally {
        Pop-Location
    }

    podman logs $postgresOwnedId *> $postgresLog
    if ($LASTEXITCODE -ne 0) { throw "PostgreSQL smoke log capture failed." }
    podman logs $redisOwnedId *> $redisLog
    if ($LASTEXITCODE -ne 0) { throw "Redis smoke log capture failed." }

    $postgresTestOk = Select-String -Path $testLog -Pattern "postgres_store_roundtrip_when_configured ... ok" -SimpleMatch
    $redisTestOk = Select-String -Path $testLog -Pattern "redis_store_roundtrip_when_configured ... ok" -SimpleMatch
    $postgresIdemOk = Select-String -Path $testLog -Pattern "postgres_compensation_idempotency_roundtrip_when_configured ... ok" -SimpleMatch
    $redisIdemOk = Select-String -Path $testLog -Pattern "redis_compensation_idempotency_roundtrip_when_configured ... ok" -SimpleMatch
    $postgresPurgeOk = Select-String -Path $testLog -Pattern "postgres_compensation_idempotency_purge_when_configured ... ok" -SimpleMatch
    $redisTtlOk = Select-String -Path $testLog -Pattern "redis_compensation_idempotency_ttl_when_configured ... ok" -SimpleMatch

    if (-not $postgresTestOk) {
        throw "Missing PostgreSQL adapter test success marker in smoke log."
    }
    if (-not $redisTestOk) {
        throw "Missing Redis adapter test success marker in smoke log."
    }
    if (-not $postgresIdemOk) {
        throw "Missing PostgreSQL compensation idempotency test success marker in smoke log."
    }
    if (-not $redisIdemOk) {
        throw "Missing Redis compensation idempotency test success marker in smoke log."
    }
    if (-not $postgresPurgeOk) {
        throw "Missing PostgreSQL idempotency purge test success marker in smoke log."
    }
    if (-not $redisTtlOk) {
        throw "Missing Redis idempotency TTL test success marker in smoke log."
    }

 } catch {
    $primaryFailure = $_
    throw
} finally {
    [Environment]::SetEnvironmentVariable("RANVIER_PERSISTENCE_POSTGRES_URL", $originalPostgresUrl)
    [Environment]::SetEnvironmentVariable("RANVIER_PERSISTENCE_REDIS_URL", $originalRedisUrl)
    $cleanupFailures = @()
    foreach ($ownedId in @($postgresOwnedId, $redisOwnedId)) {
        if ($ownedId) {
            try {
                podman rm -f $ownedId | Out-Null
                if ($LASTEXITCODE -ne 0) { throw "Owned smoke container cleanup failed: $ownedId" }
            } catch { $cleanupFailures += $_.Exception.Message }
        }
    }
    if ($cleanupFailures.Count) {
        if ($primaryFailure) { Write-Warning ($cleanupFailures -join "; ") }
        else { throw ($cleanupFailures -join "; ") }
    }
}

Write-Host "Persistence adapter smoke passed."
Write-Host "TEST_LOG=$testLog"
Write-Host "POSTGRES_LOG=$postgresLog"
Write-Host "REDIS_LOG=$redisLog"
