param([string]$EvidenceDir = (Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString('N'))))
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
$identity = Join-Path $EvidenceDir 'dummy-machine-key'
'test fixture, not a private key' | Set-Content $identity
$script:wrapper = Join-Path $PSScriptRoot '../persistence_adapter_smoke_ssh_podman.ps1'
$savedEnvironment = @{}
foreach ($name in @('CONTAINER_HOST','CONTAINER_CONNECTION','CONTAINER_SSHKEY')) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
    [Environment]::SetEnvironmentVariable($name, $null)
}
function global:podman {
    $global:sshSafety.calls.Add(($args -join ' '))
    $global:LASTEXITCODE = 0
    if ($args[0] -eq 'machine' -and $args[1] -eq 'inspect') {
        if ($global:sshSafety.mode -eq 'inspect-error') { $global:LASTEXITCODE = 125; return }
        $user = if ($global:sshSafety.mode -eq 'invalid-user') { 'user -F injected' } else { 'user' }
        @{ Name='podman-machine-default'; State='running'; Rootful=$true; SSHConfig=@{ RemoteUsername=$user; IdentityPath=$global:sshSafety.identity; Port=14757 } } | ConvertTo-Json -Compress
    } elseif ($args[0] -eq 'system') {
        if ($global:sshSafety.mode -eq 'connection-error') { $global:LASTEXITCODE = 125; return }
        if ($global:sshSafety.mode -eq 'no-default') { '[]'; return }
        $name = if ($global:sshSafety.mode -eq 'wrong-machine') { 'other-root' } else { 'podman-machine-default-root' }
        @{ Name=$name; URI='ssh://root@127.0.0.1:14757/run/podman/podman.sock'; Identity=$global:sshSafety.identity; Default=$true } | ConvertTo-Json -Compress
    } elseif ($args[0] -eq 'machine' -and $args[1] -eq 'ssh') {
        'invalid host key'
    } else { throw 'Unexpected engine mutation in wrapper negative fixture.' }
}
try {
    $cases = @{
        'inspect-error'='machine inspection failed'; 'connection-error'='connection query failed';
        'no-default'='one default Podman connection'; 'wrong-machine'='does not match';
        'invalid-user'='Invalid machine SSH username'; 'bad-host-key'='one Ed25519';
        'override'='connection overrides'; 'occupied-port'='unavailable'
    }
    foreach ($mode in $cases.Keys) {
        $global:sshSafety=@{ mode=$mode; identity=$identity; calls=[Collections.Generic.List[string]]::new() }
        $one=[Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0)
        $two=[Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0)
        $one.Start(); $two.Start()
        $portOne=$one.LocalEndpoint.Port; $portTwo=$two.LocalEndpoint.Port
        if ($mode -ne 'occupied-port') { $one.Stop() }
        $two.Stop()
        if ($mode -eq 'override') { $env:CONTAINER_HOST='ssh://unapproved.invalid' }
        $failure=$null
        try { & $script:wrapper -PostgresPort $portOne -RedisPort $portTwo -EvidenceDir (Join-Path $EvidenceDir $mode) }
        catch { $failure=$_.Exception.Message }
        finally { $one.Stop(); $two.Stop(); $env:CONTAINER_HOST=$null }
        if (-not $failure -or $failure -notlike "*$($cases[$mode])*") { throw "Wrong wrapper rejection for $mode : $failure" }
        if ($mode -in @('override','occupied-port') -and $global:sshSafety.calls.Count) { throw 'Failed local preflight reached Podman.' }
        Write-Host "PASS: SSH wrapper $mode"
    }
    Write-Host 'PASS: 8 SSH wrapper negative cases; no real SSH or container launched'
} finally {
    foreach ($name in $savedEnvironment.Keys) { [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name]) }
    Remove-Item Function:\podman -ErrorAction SilentlyContinue
    Remove-Variable sshSafety -Scope Global -ErrorAction SilentlyContinue
}
