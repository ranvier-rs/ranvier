param(
    [Parameter(Mandatory)][string]$EvidenceDir,
    [ValidateSet('grpc', 'http/protobuf')][string]$Protocol = 'http/protobuf',
    [ValidateSet('rejected', 'stalled')][string]$Scenario = 'rejected',
    [ValidateRange(1000, 60000)][int]$DeadlineMilliseconds = 20000
)

# Requires PowerShell 7: child environment and arguments do not mutate the host.
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$destination = [IO.Path]::GetFullPath($EvidenceDir)
if (Test-Path -LiteralPath $destination) { throw 'Evidence directory already exists.' }
if ($PSVersionTable.PSVersion.Major -lt 7) { throw 'PowerShell 7 is required.' }
$source = (& git -C $root rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify source commit.' }
Push-Location $root
try {
    & cargo build --locked --offline -p ranvier-observe --example otlp_smoke --features otlp
    if ($LASTEXITCODE -ne 0) { throw 'Finite fixture build failed.' }
} finally { Pop-Location }
$target = $env:CARGO_TARGET_DIR
if ([string]::IsNullOrWhiteSpace($target)) { $target = Join-Path $root 'target' }
if (-not [IO.Path]::IsPathRooted($target)) { $target = Join-Path $root $target }
$executable = Join-Path $target 'debug/examples/otlp_smoke.exe'
if (-not (Test-Path -LiteralPath $executable)) { throw 'Built fixture executable is missing.' }
[IO.Directory]::CreateDirectory($destination) | Out-Null
$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$process = $null
$watch = [Diagnostics.Stopwatch]::new()
$naturalExit = $false
$pendingConnection = $false
try {
    $listener.Start()
    $port = $listener.LocalEndpoint.Port
    if ($Scenario -eq 'rejected') { $listener.Stop() }
    $start = [Diagnostics.ProcessStartInfo]::new($executable)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.WorkingDirectory = $root
    $start.Environment['RANVIER_OTLP_ENDPOINT'] = "http://127.0.0.1:$port"
    $start.Environment['RANVIER_OTLP_PROTOCOL'] = $Protocol
    $start.Environment['RANVIER_TELEMETRY_REDACT_MODE'] = 'public'
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    $watch.Start()
    if (-not $process.Start()) { throw 'Fixture process did not start.' }
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $naturalExit = $process.WaitForExit($DeadlineMilliseconds)
    if (-not $naturalExit) {
        $process.Kill($true)
        if (-not $process.WaitForExit(5000)) { throw 'Owned fixture failed to stop after watchdog termination.' }
    }
    $watch.Stop()
    if ($Scenario -eq 'stalled') { $pendingConnection = $listener.Pending() }
    $stdout = $stdoutTask.GetAwaiter().GetResult()
    $stderr = $stderrTask.GetAwaiter().GetResult()
    [IO.File]::WriteAllText((Join-Path $destination 'stdout.log'), $stdout)
    [IO.File]::WriteAllText((Join-Path $destination 'stderr.log'), $stderr)
    $passed = $naturalExit -and $process.ExitCode -ne 0 -and
        $stdout -notmatch 'OTLP_SMOKE_EXPORTED' -and
        $stderr -match 'OTLP provider shutdown failed:.*Traces.*Metrics' -and
        ($Scenario -ne 'stalled' -or $pendingConnection)
    [ordered]@{
        source = $source; executable_sha256 = (Get-FileHash -LiteralPath $executable).Hash
        protocol = $Protocol; scenario = $Scenario; process_id = $process.Id
        natural_exit = $naturalExit; exit_code = $process.ExitCode
        elapsed_ms = $watch.ElapsedMilliseconds; deadline_ms = $DeadlineMilliseconds
        has_exited = $process.HasExited; pending_connection = $pendingConnection
        status = $(if ($passed) { 'PASS' } else { 'FAIL' })
    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $destination 'result.json') -Encoding utf8
    if (-not $passed) { throw 'Failure exit assertions failed; inspect preserved evidence.' }
} finally {
    $listener.Stop()
    if ($null -ne $process) {
        try {
            if (-not $process.HasExited) {
                $process.Kill($true)
                if (-not $process.WaitForExit(5000)) { throw 'Owned process cleanup failed.' }
            }
        } finally { $process.Dispose() }
    }
}
Write-Host "OTLP failure exit PASS: $Protocol / $Scenario"
