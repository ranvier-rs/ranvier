param(
    [ValidateSet("m119", "m131", "all")]
    [string]$Profile = "all",
    [switch]$NoAllowDirty,
    [switch]$ExecuteNextWaveDryRun,
    [string]$TargetVersion,
    [string]$EvidenceDir = "..\docs\05_dev_plans\evidence"
)

$ErrorActionPreference = "Stop"

function Write-Log {
    param(
        [string]$Path,
        [string]$Message
    )
    $line = "[{0}] {1}" -f (Get-Date -Format "HH:mm:ss"), $Message
    Write-Host $line
    Add-Content -Path $Path -Value $line -Encoding utf8
}

function Resolve-TargetVersion {
    param(
        [string]$ProfileKey,
        [string]$Requested
    )

    if (-not [string]::IsNullOrWhiteSpace($Requested)) {
        return $Requested.Trim()
    }

    switch ($ProfileKey) {
        "m119" { return "0.2.0" }
        "m131" { return "0.7.0" }
        "all" { return "0.2.0" }
        default { return "" }
    }
}

function Resolve-PowerShellExecutable {
    $pwsh = Get-Command pwsh -ErrorAction SilentlyContinue
    if ($null -ne $pwsh) {
        return $pwsh.Source
    }

    $powershell = Get-Command powershell -ErrorAction SilentlyContinue
    if ($null -ne $powershell) {
        return $powershell.Source
    }

    throw "No PowerShell executable found (pwsh/powershell)."
}

function Get-LatestFile {
    param(
        [string]$Directory,
        [string]$Pattern,
        [datetime]$SinceUtc
    )

    $items = Get-ChildItem -Path $Directory -Filter $Pattern -File -ErrorAction SilentlyContinue |
        Where-Object { $_.LastWriteTimeUtc -ge $SinceUtc } |
        Sort-Object LastWriteTimeUtc -Descending

    if ($null -eq $items -or $items.Count -eq 0) {
        return $null
    }

    return $items[0]
}

function As-Array {
    param([object]$Value)

    if ($null -eq $Value) {
        return [object[]]@()
    }

    if ($Value -is [string]) {
        return [object[]]@($Value)
    }

    if ($Value -is [pscustomobject] -or $Value -is [hashtable]) {
        return [object[]]@($Value)
    }

    if ($Value -is [System.Array]) {
        return [object[]]@($Value)
    }

    if ($Value -is [System.Collections.IEnumerable]) {
        return [object[]]@($Value)
    }

    return [object[]]@($Value)
}

$workspaceRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$profileKey = $Profile.ToLowerInvariant()
$target = Resolve-TargetVersion -ProfileKey $profileKey -Requested $TargetVersion
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$psExe = Resolve-PowerShellExecutable

New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
$bundleLogPath = Join-Path $EvidenceDir "release_gate_bundle_${profileKey}_${timestamp}.log"
$bundleSummaryPath = Join-Path $EvidenceDir "release_gate_bundle_${profileKey}_${timestamp}.json"

Write-Log -Path $bundleLogPath -Message "release gate bundle started (profile=$profileKey, no_allow_dirty=$($NoAllowDirty.IsPresent), target=$target)"
Write-Log -Path $bundleLogPath -Message "workspace root: $workspaceRoot"
Write-Log -Path $bundleLogPath -Message "powershell executable: $psExe"

$preflightScript = Join-Path $PSScriptRoot "publish_dry_run_preflight.ps1"
$waveScript = Join-Path $PSScriptRoot "plan_publish_waves.ps1"
$registryScript = Join-Path $PSScriptRoot "check_cratesio_versions.ps1"
$nextWaveScript = Join-Path $PSScriptRoot "plan_next_publish_wave.ps1"
$nextWaveExecuteScript = Join-Path $PSScriptRoot "execute_next_publish_wave.ps1"

$preflightStart = [datetime]::UtcNow
$preflightArgs = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $preflightScript, "-Profile", $profileKey)
if ($NoAllowDirty.IsPresent) {
    $preflightArgs += "-NoAllowDirty"
}

Write-Log -Path $bundleLogPath -Message "running preflight script..."
& $psExe @preflightArgs
$preflightExitCode = $LASTEXITCODE
Write-Log -Path $bundleLogPath -Message "preflight exit_code=$preflightExitCode"

$preflightSummaryFile = Get-LatestFile -Directory $EvidenceDir -Pattern "publish_dry_run_preflight_${profileKey}_*.json" -SinceUtc $preflightStart
if ($null -eq $preflightSummaryFile) {
    throw "Failed to locate preflight summary for profile=$profileKey"
}
Write-Log -Path $bundleLogPath -Message "preflight summary: $($preflightSummaryFile.FullName)"

$waveStart = [datetime]::UtcNow
$waveArgs = @(
    "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $waveScript,
    "-Profile", $profileKey,
    "-SummaryPath", $preflightSummaryFile.FullName
)

Write-Log -Path $bundleLogPath -Message "running wave planner..."
& $psExe @waveArgs
$waveExitCode = $LASTEXITCODE
Write-Log -Path $bundleLogPath -Message "wave planner exit_code=$waveExitCode"

$waveSummaryFile = Get-LatestFile -Directory $EvidenceDir -Pattern "publish_wave_plan_${profileKey}_*.json" -SinceUtc $waveStart
if ($null -eq $waveSummaryFile) {
    throw "Failed to locate wave summary for profile=$profileKey"
}
Write-Log -Path $bundleLogPath -Message "wave summary: $($waveSummaryFile.FullName)"

$registryStart = [datetime]::UtcNow
$registryArgs = @(
    "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $registryScript,
    "-Profile", $profileKey,
    "-TargetVersion", $target
)

Write-Log -Path $bundleLogPath -Message "running crates.io registry snapshot..."
& $psExe @registryArgs
$registryExitCode = $LASTEXITCODE
Write-Log -Path $bundleLogPath -Message "registry snapshot exit_code=$registryExitCode"

$registryPattern = "cratesio_version_snapshot_${profileKey}_${target}_*.json"
$registrySummaryFile = Get-LatestFile -Directory $EvidenceDir -Pattern $registryPattern -SinceUtc $registryStart
if ($null -eq $registrySummaryFile) {
    throw "Failed to locate registry snapshot summary for profile=$profileKey target=$target"
}
Write-Log -Path $bundleLogPath -Message "registry summary: $($registrySummaryFile.FullName)"

$nextWaveStart = [datetime]::UtcNow
$nextWaveArgs = @(
    "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $nextWaveScript,
    "-Profile", $profileKey,
    "-TargetVersion", $target,
    "-WaveSummaryPath", $waveSummaryFile.FullName,
    "-RegistrySummaryPath", $registrySummaryFile.FullName
)

Write-Log -Path $bundleLogPath -Message "running next-wave gate planner..."
& $psExe @nextWaveArgs
$nextWaveExitCode = $LASTEXITCODE
Write-Log -Path $bundleLogPath -Message "next-wave gate planner exit_code=$nextWaveExitCode"

$nextWavePattern = "publish_next_wave_gate_${profileKey}_${target}_*.json"
$nextWaveSummaryFile = Get-LatestFile -Directory $EvidenceDir -Pattern $nextWavePattern -SinceUtc $nextWaveStart
if ($null -eq $nextWaveSummaryFile) {
    throw "Failed to locate next-wave gate summary for profile=$profileKey target=$target"
}
Write-Log -Path $bundleLogPath -Message "next-wave gate summary: $($nextWaveSummaryFile.FullName)"

$nextWaveExecuteExitCode = 0
$nextWaveExecuteSummaryFile = $null
if ($ExecuteNextWaveDryRun.IsPresent) {
    $nextWaveExecuteStart = [datetime]::UtcNow
    $nextWaveExecuteArgs = @(
        "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $nextWaveExecuteScript,
        "-Profile", $profileKey,
        "-TargetVersion", $target,
        "-Mode", "dry-run",
        "-NextWaveSummaryPath", $nextWaveSummaryFile.FullName
    )
    if (-not $NoAllowDirty.IsPresent) {
        $nextWaveExecuteArgs += "-AllowDirty"
    }

    Write-Log -Path $bundleLogPath -Message "running next-wave execution (dry-run)..."
    & $psExe @nextWaveExecuteArgs
    $nextWaveExecuteExitCode = $LASTEXITCODE
    Write-Log -Path $bundleLogPath -Message "next-wave execution exit_code=$nextWaveExecuteExitCode"

    $nextWaveExecutePattern = "publish_next_wave_execute_${profileKey}_${target}_*.json"
    $nextWaveExecuteSummaryFile = Get-LatestFile -Directory $EvidenceDir -Pattern $nextWaveExecutePattern -SinceUtc $nextWaveExecuteStart
    if ($null -eq $nextWaveExecuteSummaryFile) {
        throw "Failed to locate next-wave execute summary for profile=$profileKey target=$target"
    }
    Write-Log -Path $bundleLogPath -Message "next-wave execute summary: $($nextWaveExecuteSummaryFile.FullName)"
}

$preflightSummary = Get-Content -Path $preflightSummaryFile.FullName -Raw | ConvertFrom-Json
$waveSummary = Get-Content -Path $waveSummaryFile.FullName -Raw | ConvertFrom-Json
$registrySummary = Get-Content -Path $registrySummaryFile.FullName -Raw | ConvertFrom-Json
$nextWaveSummary = Get-Content -Path $nextWaveSummaryFile.FullName -Raw | ConvertFrom-Json
$nextWaveExecuteSummary = $null
if ($null -ne $nextWaveExecuteSummaryFile) {
    $nextWaveExecuteSummary = Get-Content -Path $nextWaveExecuteSummaryFile.FullName -Raw | ConvertFrom-Json
}

$wave1Crates = @(
    $waveSummary.waves |
        Select-Object -First 1 |
        ForEach-Object { $_.crates } |
        ForEach-Object { $_ } |
        ForEach-Object { [string]$_ } |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
)
$publishCommandsWave1 = @($wave1Crates | ForEach-Object { "cargo publish -p $_" })

$bundleSummary = [ordered]@{
    timestamp = $timestamp
    profile = $profileKey
    target_version = $target
    no_allow_dirty = $NoAllowDirty.IsPresent
    preflight = [ordered]@{
        exit_code = $preflightExitCode
        summary_path = $preflightSummaryFile.FullName
        passed = [int]$preflightSummary.passed
        failed = [int]$preflightSummary.failed
        failed_crates = @(As-Array $preflightSummary.failed_crates | ForEach-Object { [string]$_ })
    }
    wave_plan = [ordered]@{
        exit_code = $waveExitCode
        summary_path = $waveSummaryFile.FullName
        wave_count = [int]$waveSummary.wave_count
        blocked_count = [int]$waveSummary.blocked_count
        wave1_crates = $wave1Crates
        publish_commands_wave1 = $publishCommandsWave1
    }
    registry_snapshot = [ordered]@{
        exit_code = $registryExitCode
        summary_path = $registrySummaryFile.FullName
        found = [int]$registrySummary.found
        not_found = [int]$registrySummary.not_found
        target_present_count = [int]$registrySummary.target_present_count
        target_missing_count = [int]$registrySummary.target_missing_count
        target_present_crates = @(As-Array $registrySummary.target_present_crates | ForEach-Object { [string]$_ })
        target_missing_crates = @(As-Array $registrySummary.target_missing_crates | ForEach-Object { [string]$_ })
        not_found_crates = @(As-Array $registrySummary.not_found_crates | ForEach-Object { [string]$_ })
    }
    next_publish_gate = [ordered]@{
        exit_code = $nextWaveExitCode
        summary_path = $nextWaveSummaryFile.FullName
        all_waves_complete = [bool]$nextWaveSummary.all_waves_complete
        next_publish_wave = $nextWaveSummary.next_publish_wave
        next_publish_crates = @(As-Array $nextWaveSummary.next_publish_crates | ForEach-Object { [string]$_ })
        next_publish_commands = @(As-Array $nextWaveSummary.next_publish_commands | ForEach-Object { [string]$_ })
    }
    next_publish_execute = [ordered]@{
        enabled = $ExecuteNextWaveDryRun.IsPresent
        mode = if ($ExecuteNextWaveDryRun.IsPresent) { "dry-run" } else { $null }
        exit_code = if ($ExecuteNextWaveDryRun.IsPresent) { $nextWaveExecuteExitCode } else { $null }
        summary_path = if ($null -ne $nextWaveExecuteSummaryFile) { $nextWaveExecuteSummaryFile.FullName } else { $null }
        skipped = if ($null -ne $nextWaveExecuteSummary) { [bool]$nextWaveExecuteSummary.skipped } else { $null }
        next_publish_wave = if ($null -ne $nextWaveExecuteSummary) { $nextWaveExecuteSummary.next_publish_wave } else { $null }
        next_publish_crates = if ($null -ne $nextWaveExecuteSummary) { @(As-Array $nextWaveExecuteSummary.next_publish_crates | ForEach-Object { [string]$_ }) } else { @() }
    }
}

$bundleSummary | ConvertTo-Json -Depth 8 | Set-Content -Path $bundleSummaryPath -Encoding utf8

Write-Log -Path $bundleLogPath -Message "wave1 crates: $($wave1Crates -join ', ')"
Write-Log -Path $bundleLogPath -Message "registry target_present_count=$($bundleSummary.registry_snapshot.target_present_count)"
Write-Log -Path $bundleLogPath -Message "next publish wave=$($bundleSummary.next_publish_gate.next_publish_wave)"
if ($ExecuteNextWaveDryRun.IsPresent) {
    Write-Log -Path $bundleLogPath -Message "next-wave execution enabled: exit_code=$nextWaveExecuteExitCode"
}
Write-Log -Path $bundleLogPath -Message "bundle summary: $bundleSummaryPath"

Write-Host "Evidence: $bundleLogPath"
Write-Host "Summary:  $bundleSummaryPath"

if ($preflightExitCode -ne 0) {
    exit $preflightExitCode
}

if ($ExecuteNextWaveDryRun.IsPresent -and $nextWaveExecuteExitCode -ne 0) {
    exit $nextWaveExecuteExitCode
}
