param([string]$EvidenceDir = (Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString('N'))))
$ErrorActionPreference = 'Stop'
$smokeScript = Join-Path $PSScriptRoot '../persistence_adapter_smoke_podman.ps1'
$originalPg = [Environment]::GetEnvironmentVariable('RANVIER_PERSISTENCE_POSTGRES_URL')
$originalRedis = [Environment]::GetEnvironmentVariable('RANVIER_PERSISTENCE_REDIS_URL')

function global:podman {
    $call = @($args)
    $global:smokeSafety.calls.Add(($call -join ' '))
    $global:LASTEXITCODE = 0
    switch ($call[0]) {
        'container' {
            $global:LASTEXITCODE = switch ($global:smokeSafety.mode) { 'present' { 0 } 'query-error' { 125 } default { 1 } }
        }
        'run' {
            $global:smokeSafety.starts++
            if ($global:smokeSafety.mode -eq 'second-start-fail' -and $global:smokeSafety.starts -eq 2) { $global:LASTEXITCODE = 125; return }
            if ($global:smokeSafety.mode -eq 'invalid-id') { 'not-an-owned-id'; return }
            if ($global:smokeSafety.starts -eq 1) { 'a' * 64 } else { 'b' * 64 }
        }
        'exec' { if ($call -contains 'ping') { 'PONG' } }
        'inspect' { if ($global:smokeSafety.mode -eq 'image-query-error') { $global:LASTEXITCODE = 125 } else { 'c' * 64 } }
        'logs' {
            if ($global:smokeSafety.mode -in @('logs-error', 'logs-and-cleanup-error')) { $global:LASTEXITCODE = 125 } else { 'mock log' }
        }
        'rm' {
            $global:smokeSafety.removed.Add($call[2])
            if ($global:smokeSafety.mode -in @('cleanup-error', 'logs-and-cleanup-error') -and $call[2] -eq ('a' * 64)) { $global:LASTEXITCODE = 125 }
        }
        default { throw "Unexpected mocked podman call: $call" }
    }
}

function global:cargo {
    if ($args -notcontains '--locked') { throw 'Smoke Cargo invocation must be locked' }
    foreach ($marker in @(
        'postgres_store_roundtrip_when_configured', 'redis_store_roundtrip_when_configured',
        'postgres_compensation_idempotency_roundtrip_when_configured', 'redis_compensation_idempotency_roundtrip_when_configured',
        'postgres_compensation_idempotency_purge_when_configured', 'redis_compensation_idempotency_ttl_when_configured'
    )) { "test $marker ... ok" }
    $global:LASTEXITCODE = 0
}

try {
    foreach ($mode in @('present', 'query-error', 'success', 'second-start-fail', 'logs-error', 'cleanup-error', 'logs-and-cleanup-error', 'invalid-id', 'image-query-error')) {
        $global:smokeSafety = @{ mode = $mode; starts = 0; calls = [Collections.Generic.List[string]]::new(); removed = [Collections.Generic.List[string]]::new() }
        $env:RANVIER_PERSISTENCE_POSTGRES_URL = 'existing-pg'
        $env:RANVIER_PERSISTENCE_REDIS_URL = 'existing-redis'
        $failure = $null
        try { & $smokeScript -EvidenceDir (Join-Path $EvidenceDir $mode) -StartupWaitSeconds 0 }
        catch { $failure = $_.Exception.Message }
        if (($mode -eq 'success') -ne ($null -eq $failure)) { throw "Wrong success classification for $mode : $failure" }
        $expected = switch ($mode) {
            'present' { 'already exists' } 'query-error' { 'existence query failed' }
            'second-start-fail' { 'podman run failed' } 'logs-error' { 'log capture failed' }
            'cleanup-error' { 'cleanup failed' } 'logs-and-cleanup-error' { 'log capture failed' } 'invalid-id' { 'valid owned container ID' } 'image-query-error' { 'image identity query failed' } default { '' }
        }
        if ($expected -and $failure -notlike "*$expected*") { throw "Wrong failure diagnostic for $mode : $failure" }
        $ownedCount = switch ($mode) { 'present' { 0 } 'query-error' { 0 } 'second-start-fail' { 1 } 'invalid-id' { 0 } default { 2 } }
        if ($global:smokeSafety.removed.Count -ne $ownedCount) { throw "Owned cleanup count wrong for $mode" }
        foreach ($id in $global:smokeSafety.removed) { if ($id -notmatch '^[ab]{64}$') { throw "Cleanup used a name instead of owned ID: $id" } }
        if ($mode -in @('present', 'query-error') -and $global:smokeSafety.starts) { throw "Failed preflight started a container: $mode" }
        foreach ($call in $global:smokeSafety.calls | Where-Object { $_ -like 'run *' }) {
            if ($call -notlike '*-p 127.0.0.1:*' -or $call -notlike '*--rm*') { throw "Non-isolated run: $call" }
        }
        if ($env:RANVIER_PERSISTENCE_POSTGRES_URL -ne 'existing-pg' -or $env:RANVIER_PERSISTENCE_REDIS_URL -ne 'existing-redis') { throw "Environment was not restored: $mode" }
        Write-Host "PASS: $mode"
    }
    Write-Host 'PASS: 9 mocked persistence smoke safety cases; no live runtime exercised'
} finally {
    [Environment]::SetEnvironmentVariable('RANVIER_PERSISTENCE_POSTGRES_URL', $originalPg)
    [Environment]::SetEnvironmentVariable('RANVIER_PERSISTENCE_REDIS_URL', $originalRedis)
    Remove-Item Function:\podman, Function:\cargo -ErrorAction SilentlyContinue
    Remove-Variable smokeSafety -Scope Global -ErrorAction SilentlyContinue
}
