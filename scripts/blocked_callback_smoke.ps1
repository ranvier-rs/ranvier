param(
    [Parameter(Mandatory)][string]$EvidenceDir,
    [ValidateRange(8000, 60000)][int]$DeadlineMilliseconds = 12000
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if ($PSVersionTable.PSVersion.Major -lt 7) { throw 'PowerShell 7 required.' }
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$destination = [IO.Path]::GetFullPath($EvidenceDir)
if (Test-Path -LiteralPath $destination) { throw 'Evidence directory already exists.' }
$source = (& git -C $root rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify source commit.' }
Push-Location $root
try {
    & cargo build --locked --offline -p ranvier-observe --features otlp --example blocked_callback
    if ($LASTEXITCODE -ne 0) { throw 'Fixture build failed.' }
} finally { Pop-Location }
$target = $env:CARGO_TARGET_DIR
if ([string]::IsNullOrWhiteSpace($target)) { $target = Join-Path $root 'target' }
if (-not [IO.Path]::IsPathRooted($target)) { $target = Join-Path $root $target }
$executable = Join-Path $target 'debug/examples/blocked_callback.exe'
if (-not (Test-Path -LiteralPath $executable)) { throw 'Fixture executable missing.' }
[IO.Directory]::CreateDirectory($destination) | Out-Null
$process = [Diagnostics.Process]::new()
$started = $false
try {
    $start = [Diagnostics.ProcessStartInfo]::new($executable)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.WorkingDirectory = $root
    $process.StartInfo = $start
    if (-not $process.Start()) { throw 'Fixture failed to start.' }
    $started = $true
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $naturalExit = $process.WaitForExit($DeadlineMilliseconds)
    if (-not $naturalExit) {
        $process.Kill($true)
        if (-not $process.WaitForExit(5000)) { throw 'Owned fixture failed to stop.' }
    }
    $stdout = $stdoutTask.GetAwaiter().GetResult()
    $stderr = $stderrTask.GetAwaiter().GetResult()
    [IO.File]::WriteAllText((Join-Path $destination 'stdout.log'), $stdout)
    [IO.File]::WriteAllText((Join-Path $destination 'stderr.log'), $stderr)
    $passed = -not $naturalExit -and $process.HasExited -and
        $stdout.Contains('CALLBACK_ENTERED') -and
        $stdout.Contains('SHUTDOWN_RETURNED_METRICS_FAILURE') -and
        $stdout.Contains('CALLBACK_STILL_ACTIVE') -and
        [string]::IsNullOrWhiteSpace($stderr)
    $shutdownElapsed = $null
    if ($stdout -match 'SHUTDOWN_RETURNED_METRICS_FAILURE elapsed_ms=(\d+)') {
        $shutdownElapsed = [long]$Matches[1]
    }
    [ordered]@{
        source=$source; executable_sha256=(Get-FileHash -LiteralPath $executable).Hash
        fixture_sha256=(Get-FileHash -LiteralPath (Join-Path $root 'extensions/observe/examples/blocked_callback.rs')).Hash
        harness_sha256=(Get-FileHash -LiteralPath $PSCommandPath).Hash
        cargo_lock_sha256=(Get-FileHash -LiteralPath (Join-Path $root 'Cargo.lock')).Hash
        toolchain=(& rustc --version | Out-String).Trim()
        deadline_ms=$DeadlineMilliseconds; process_id=$process.Id
        sdk_shutdown_elapsed_ms=$shutdownElapsed
        natural_exit=$naturalExit; has_exited=$process.HasExited
        classification='SDK_SHUTDOWN_RETURNED_WITH_BLOCKED_WORKER'
        status=$(if ($passed) { 'PASS' } else { 'FAIL' })
    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $destination 'result.json') -Encoding utf8
    if (-not $passed) { throw 'Blocked callback assertions failed; inspect evidence.' }
} finally {
    try {
        if ($started -and -not $process.HasExited) {
            $process.Kill($true)
            if (-not $process.WaitForExit(5000)) { throw 'Owned process cleanup failed.' }
        }
    } finally { $process.Dispose() }
}
Write-Host 'Blocked callback reproduction PASS; watchdog terminated owned process.'
