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
    $global:smokeSafety.cargoCalls++
    if ($args -notcontains '--locked') { throw 'Smoke Cargo invocation must be locked' }
    foreach ($marker in @(
        'postgres_store_roundtrip_when_configured', 'redis_store_roundtrip_when_configured',
        'postgres_compensation_idempotency_roundtrip_when_configured', 'redis_compensation_idempotency_roundtrip_when_configured',
        'postgres_compensation_idempotency_purge_when_configured', 'redis_compensation_idempotency_ttl_when_configured'
    )) { "test $marker ... ok" }
    $global:LASTEXITCODE = 0
}

try {
    foreach ($mode in @('present', 'query-error', 'success', 'second-start-fail', 'logs-error', 'cleanup-error', 'logs-and-cleanup-error', 'invalid-id', 'image-query-error', 'host-unreachable', 'forwarder-exited')) {
        $global:smokeSafety = @{ mode = $mode; starts = 0; cargoCalls = 0; calls = [Collections.Generic.List[string]]::new(); removed = [Collections.Generic.List[string]]::new() }
        $env:RANVIER_PERSISTENCE_POSTGRES_URL = 'existing-pg'
        $env:RANVIER_PERSISTENCE_REDIS_URL = 'existing-redis'
        $failure = $null
        $listenerPg = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
        $listenerRedis = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
        $listenerPg.Start(); $listenerRedis.Start()
        $portPg = $listenerPg.LocalEndpoint.Port; $portRedis = $listenerRedis.LocalEndpoint.Port
        if ($mode -eq 'host-unreachable') { $listenerPg.Stop(); $listenerRedis.Stop() }
        $forwarder = $null
        if ($mode -eq 'forwarder-exited') {
            $forwarder = Start-Process -FilePath (Join-Path $env:SystemRoot 'System32/cmd.exe') -ArgumentList '/c', 'exit', '0' -WindowStyle Hidden -PassThru
            if (-not $forwarder.WaitForExit(5000)) { $forwarder.Kill(); throw 'Forwarder control process did not exit.' }
        }
        try { & $smokeScript -EvidenceDir (Join-Path $EvidenceDir $mode) -StartupWaitSeconds 0 -PostgresPort $portPg -RedisPort $portRedis -ForwarderProcess $forwarder }
        catch { $failure = $_.Exception.Message }
        finally { $listenerPg.Stop(); $listenerRedis.Stop(); if ($forwarder) { $forwarder.Dispose() } }
        if (($mode -eq 'success') -ne ($null -eq $failure)) { throw "Wrong success classification for $mode : $failure" }
        $expected = switch ($mode) {
            'present' { 'already exists' } 'query-error' { 'existence query failed' }
            'second-start-fail' { 'podman run failed' } 'logs-error' { 'log capture failed' }
            'cleanup-error' { 'cleanup failed' } 'logs-and-cleanup-error' { 'log capture failed' } 'invalid-id' { 'valid owned container ID' } 'image-query-error' { 'image identity query failed' } 'host-unreachable' { 'Host loopback endpoint not reachable' } 'forwarder-exited' { 'Owned forwarder exited' } default { '' }
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
        if ($mode -in @('host-unreachable', 'forwarder-exited') -and $global:smokeSafety.cargoCalls) { throw 'Unreachable host endpoint launched Cargo' }
        Write-Host "PASS: $mode"
    }
    Write-Host 'PASS: 11 mocked persistence smoke safety cases; no live runtime exercised'
} finally {
    [Environment]::SetEnvironmentVariable('RANVIER_PERSISTENCE_POSTGRES_URL', $originalPg)
    [Environment]::SetEnvironmentVariable('RANVIER_PERSISTENCE_REDIS_URL', $originalRedis)
    Remove-Item Function:\podman, Function:\cargo -ErrorAction SilentlyContinue
    Remove-Variable smokeSafety -Scope Global -ErrorAction SilentlyContinue
}
