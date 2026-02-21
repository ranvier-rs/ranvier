param(
    [string]$PostgresContainer = "ranvier-persistence-pg-smoke",
    [string]$RedisContainer = "ranvier-persistence-redis-smoke",
    [int]$PostgresPort = 54329,
    [int]$RedisPort = 6389,
    [int]$StartupWaitSeconds = 6
)

$ErrorActionPreference = "Stop"

function Stop-ContainerIfExists {
    param([string]$Name)

    $existing = podman ps -a --format "{{.Names}}" | Select-String -Pattern "^$Name$"
    if ($existing) {
        podman rm -f $Name | Out-Null
    }
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
$evidenceDir = Join-Path $workspaceRoot "docs/05_dev_plans/evidence"
New-Item -ItemType Directory -Path $evidenceDir -Force | Out-Null

$stamp = Get-Date -Format "yyyyMMdd_HHmmss"
$testLog = Join-Path $evidenceDir ("persistence_adapter_smoke_test_" + $stamp + ".log")
$postgresLog = Join-Path $evidenceDir ("persistence_adapter_smoke_postgres_" + $stamp + ".log")
$redisLog = Join-Path $evidenceDir ("persistence_adapter_smoke_redis_" + $stamp + ".log")

try {
    Stop-ContainerIfExists -Name $PostgresContainer
    Stop-ContainerIfExists -Name $RedisContainer

    podman run -d --name $PostgresContainer `
        -e POSTGRES_USER=ranvier `
        -e POSTGRES_PASSWORD=ranvier `
        -e POSTGRES_DB=ranvier `
        -p "${PostgresPort}:5432" `
        postgres:16-alpine | Out-Null

    podman run -d --name $RedisContainer `
        -p "${RedisPort}:6379" `
        redis:7-alpine | Out-Null

    Start-Sleep -Seconds $StartupWaitSeconds
    Wait-PostgresReady -ContainerName $PostgresContainer
    Wait-RedisReady -ContainerName $RedisContainer

    $env:RANVIER_PERSISTENCE_POSTGRES_URL = "postgres://ranvier:ranvier@127.0.0.1:${PostgresPort}/ranvier"
    $env:RANVIER_PERSISTENCE_REDIS_URL = "redis://127.0.0.1:${RedisPort}"

    Push-Location $ranvierRoot
    try {
        cargo test -p ranvier-runtime --features persistence-postgres,persistence-redis *>&1 | Tee-Object -FilePath $testLog | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "cargo test failed for persistence adapter smoke."
        }
    } finally {
        Pop-Location
    }

    podman logs $PostgresContainer *> $postgresLog
    podman logs $RedisContainer *> $redisLog

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

    Write-Host "Persistence adapter smoke passed."
    Write-Host "TEST_LOG=$testLog"
    Write-Host "POSTGRES_LOG=$postgresLog"
    Write-Host "REDIS_LOG=$redisLog"
} finally {
    Stop-ContainerIfExists -Name $PostgresContainer
    Stop-ContainerIfExists -Name $RedisContainer
}
