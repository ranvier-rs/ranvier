param(
    [ValidatePattern('^[a-zA-Z0-9_-]+$')][string]$MachineName = 'podman-machine-default',
    [ValidateRange(1, 65535)][int]$PostgresPort = 54329,
    [ValidateRange(1, 65535)][int]$RedisPort = 6389,
    [string]$EvidenceDir = ''
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { throw 'This SSH transport wrapper is for Windows hosts.' }
if ($PostgresPort -eq $RedisPort) { throw 'Distinct forward ports are required.' }
if ($env:CONTAINER_HOST -or $env:CONTAINER_CONNECTION -or $env:CONTAINER_SSHKEY) { throw 'Explicit Podman connection overrides are not supported by this machine-scoped wrapper.' }
foreach ($port in @($PostgresPort, $RedisPort)) {
    $probe = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, $port)
    try { $probe.Start() } catch { throw "Local forward port $port is unavailable." } finally { $probe.Stop() }
}
if ([string]::IsNullOrWhiteSpace($EvidenceDir)) {
    $EvidenceDir = Join-Path ([IO.Path]::GetTempPath()) ('ranvier-persistence-ssh-' + [Guid]::NewGuid().ToString('N'))
}
$EvidenceDir = [IO.Path]::GetFullPath($EvidenceDir)
New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null

$machineText = podman machine inspect $MachineName | Out-String
if ($LASTEXITCODE -ne 0) { throw 'Podman machine inspection failed.' }
$machines = @($machineText | ConvertFrom-Json)
if ($machines.Count -ne 1 -or $machines[0].Name -ne $MachineName -or $machines[0].State -ne 'running') { throw 'Expected one running selected machine.' }
$machine = $machines[0]
if ($machine.SSHConfig.RemoteUsername -notmatch '^[a-z_][a-z0-9_-]*[$]?$') { throw 'Invalid machine SSH username.' }
$connectionText = podman system connection list --format json | Out-String
if ($LASTEXITCODE -ne 0) { throw 'Podman connection query failed.' }
$connections = @($connectionText | ConvertFrom-Json | Where-Object Default)
if ($connections.Count -ne 1) { throw 'Expected one default Podman connection.' }
$connection = $connections[0]
$uri = [Uri]$connection.URI
$expectedName = if ($machine.Rootful) { "$MachineName-root" } else { $MachineName }
$expectedUser = if ($machine.Rootful) { 'root' } else { $machine.SSHConfig.RemoteUsername }
if ($connection.Name -ne $expectedName -or $uri.Scheme -ne 'ssh' -or $uri.Host -ne '127.0.0.1' -or
    $uri.Port -ne $machine.SSHConfig.Port -or $uri.UserInfo -ne $expectedUser -or
    $connection.Identity -ne $machine.SSHConfig.IdentityPath) { throw 'Default Podman connection does not match selected machine.' }
$identity = [IO.Path]::GetFullPath($machine.SSHConfig.IdentityPath)
if (-not (Test-Path -LiteralPath $identity -PathType Leaf)) { throw 'Machine SSH identity is missing.' }
$hostKeyText = podman machine ssh $MachineName cat /etc/ssh/ssh_host_ed25519_key.pub | Out-String
if ($LASTEXITCODE -ne 0) { throw 'Machine host-key query failed.' }
$hostKey = $hostKeyText.Trim()
if ($hostKey -notmatch '^ssh-ed25519 [A-Za-z0-9+/]+={0,3}(?: [^\r\n]*)?$') { throw 'Expected one Ed25519 machine host key.' }
$knownHosts = Join-Path $EvidenceDir 'ssh-known-hosts'
"[127.0.0.1]:$($uri.Port) $hostKey" | Set-Content -Encoding ascii $knownHosts
$ssh = (Get-Command ssh -ErrorAction Stop).Source
$sshArguments = @(
    '-N', '-T', '-o', 'BatchMode=yes', '-o', 'ExitOnForwardFailure=yes',
    '-o', 'StrictHostKeyChecking=yes', '-o', ('UserKnownHostsFile="{0}"' -f $knownHosts),
    '-o', 'ConnectTimeout=5', '-i', ('"{0}"' -f $identity), '-p', "$($uri.Port)",
    '-L', "127.0.0.1:${PostgresPort}:127.0.0.1:${PostgresPort}",
    '-L', "127.0.0.1:${RedisPort}:127.0.0.1:${RedisPort}",
    "$($machine.SSHConfig.RemoteUsername)@127.0.0.1"
)
$tunnel = $null
$primaryFailure = $null
try {
    $tunnel = Start-Process -FilePath $ssh -ArgumentList $sshArguments -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $EvidenceDir 'ssh-stderr.log')
    Start-Sleep -Milliseconds 300
    if ($tunnel.HasExited) { throw 'SSH loopback tunnel exited during startup; inspect ssh-stderr.log.' }
    [ordered]@{ machine = $MachineName; default_connection = $connection.Name; ssh_port = $uri.Port; process_id = $tunnel.Id; known_hosts_sha256 = (Get-FileHash $knownHosts).Hash } |
        ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $EvidenceDir 'ssh-tunnel-identity.json')
    & (Join-Path $PSScriptRoot 'persistence_adapter_smoke_podman.ps1') -PostgresPort $PostgresPort -RedisPort $RedisPort -StartupWaitSeconds 0 -EvidenceDir $EvidenceDir -ForwarderProcess $tunnel
} catch {
    $primaryFailure = $_
    throw
} finally {
    if ($tunnel -and -not $tunnel.HasExited) {
        try {
            $tunnel.Kill()
            if (-not $tunnel.WaitForExit(5000)) { throw 'Owned SSH tunnel did not terminate.' }
        } catch {
            if ($primaryFailure) { Write-Warning $_.Exception.Message } else { throw }
        }
    }
}
Write-Host 'SSH persistence adapter smoke passed; owned tunnel stopped.'
