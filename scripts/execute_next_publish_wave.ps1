param(
    [ValidateSet("m119", "m131", "all")]
    [string]$Profile = "all",
    [string]$TargetVersion,
    [ValidateSet("dry-run", "publish")]
    [string]$Mode = "dry-run",
    [switch]$ConfirmPublish,
    [switch]$AllowDirty,
    [switch]$ContinueOnError,
    [switch]$SkipTokenCheck,
    [switch]$SkipCleanTreeCheck,
    [ValidateRange(0, 20)]
    [int]$RetryCount = 0,
    [ValidateRange(1, 600)]
    [int]$RetryDelaySeconds = 20,
    [string]$NextWaveSummaryPath,
    [string]$EvidenceDir = "..\docs\05_dev_plans\evidence"
)

$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "release_common.ps1")

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
        [string]$Requested,
        [string]$WorkspaceRoot
    )

    return Resolve-ReleaseTargetVersion -Requested $Requested -WorkspaceRoot $WorkspaceRoot
}

function Resolve-InputPath {
    param(
        [string]$Value,
        [string]$BasePath
    )

    if (Test-Path $Value) {
        return (Resolve-Path $Value).Path
    }

    $candidate = Join-Path $BasePath $Value
    if (Test-Path $candidate) {
        return (Resolve-Path $candidate).Path
    }

    throw "Path not found: $Value"
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

function Resolve-NextWaveSummaryPath {
    param(
        [string]$Requested,
        [string]$ProfileKey,
        [string]$Target,
        [string]$EvidenceRoot,
        [string]$WorkspaceRoot
    )

    if (-not [string]::IsNullOrWhiteSpace($Requested)) {
        return Resolve-InputPath -Value $Requested -BasePath $WorkspaceRoot
    }

    $pattern = "publish_next_wave_gate_${ProfileKey}_${Target}_*.json"
    $latest = Get-ChildItem -Path $EvidenceRoot -Filter $pattern -File -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTimeUtc -Descending |
        Select-Object -First 1

    if ($null -eq $latest) {
        throw "No next-wave gate summary found for profile=$ProfileKey target=$Target in $EvidenceRoot"
    }

    return $latest.FullName
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

$workspaceRoot = Get-ReleaseWorkspaceRoot -ScriptRoot $PSScriptRoot
$profileKey = $Profile.ToLowerInvariant()
$target = Resolve-TargetVersion -Requested $TargetVersion -WorkspaceRoot $workspaceRoot
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$psExe = Resolve-PowerShellExecutable
$EvidenceDir = Resolve-ReleaseEvidenceDir -Requested $EvidenceDir -WorkspaceRoot $workspaceRoot

New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
$evidencePath = Join-Path $EvidenceDir "publish_next_wave_execute_${profileKey}_${target}_${timestamp}.log"
$summaryOutPath = Join-Path $EvidenceDir "publish_next_wave_execute_${profileKey}_${target}_${timestamp}.json"

$resolvedNextWaveSummary = Resolve-NextWaveSummaryPath -Requested $NextWaveSummaryPath -ProfileKey $profileKey -Target $target -EvidenceRoot $EvidenceDir -WorkspaceRoot "$workspaceRoot"
$nextWaveSummary = Get-Content -Path $resolvedNextWaveSummary -Raw | ConvertFrom-Json

Write-Log -Path $evidencePath -Message "Next-wave execution started (profile=$profileKey, target=$target, mode=$Mode, confirm_publish=$($ConfirmPublish.IsPresent), allow_dirty=$($AllowDirty.IsPresent), retry_count=$RetryCount, retry_delay_seconds=$RetryDelaySeconds)"
Write-Log -Path $evidencePath -Message "Next-wave summary: $resolvedNextWaveSummary"

$allComplete = [bool]$nextWaveSummary.all_waves_complete
$nextWave = $nextWaveSummary.next_publish_wave
$nextCrates = @(@($nextWaveSummary.next_publish_crates) | ForEach-Object { [string]$_ })
$waveSummaryPath = [string]$nextWaveSummary.wave_summary_path

$executeSummaryPath = $null
$executeExitCode = 0
$skipped = $false

if ($allComplete -or $null -eq $nextWave) {
    $skipped = $true
    Write-Log -Path $evidencePath -Message "No execution required: all waves already complete or next wave not set."
} else {
    $executeScript = Join-Path $PSScriptRoot "execute_publish_wave.ps1"
    $executeStart = [datetime]::UtcNow
    $args = @(
        "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $executeScript,
        "-Profile", $profileKey,
        "-Wave", "$nextWave",
        "-Mode", $Mode,
        "-EvidenceDir", $EvidenceDir
    )

    if (-not [string]::IsNullOrWhiteSpace($waveSummaryPath)) {
        $args += @("-WaveSummaryPath", $waveSummaryPath)
    }
    if ($AllowDirty.IsPresent) {
        $args += "-AllowDirty"
    }
    if ($ContinueOnError.IsPresent) {
        $args += "-ContinueOnError"
    }
    if ($ConfirmPublish.IsPresent) {
        $args += "-ConfirmPublish"
    }
    if ($SkipTokenCheck.IsPresent) {
        $args += "-SkipTokenCheck"
    }
    if ($SkipCleanTreeCheck.IsPresent) {
        $args += "-SkipCleanTreeCheck"
    }
    $args += @("-RetryCount", "$RetryCount", "-RetryDelaySeconds", "$RetryDelaySeconds")

    Write-Log -Path $evidencePath -Message "Executing wave $nextWave crates: $($nextCrates -join ', ')"
    & $psExe @args
    $executeExitCode = $LASTEXITCODE
    Write-Log -Path $evidencePath -Message "execute_publish_wave exit_code=$executeExitCode"

    $wavePattern = "publish_wave_execute_${profileKey}_w${nextWave}_*.json"
    $executeSummaryFile = Get-LatestFile -Directory $EvidenceDir -Pattern $wavePattern -SinceUtc $executeStart
    if ($null -ne $executeSummaryFile) {
        $executeSummaryPath = $executeSummaryFile.FullName
        Write-Log -Path $evidencePath -Message "wave execution summary: $executeSummaryPath"
    } else {
        Write-Log -Path $evidencePath -Message "wave execution summary not found for pattern $wavePattern"
    }
}

$summary = [ordered]@{
    timestamp = $timestamp
    profile = $profileKey
    target_version = $target
    mode = $Mode
    confirm_publish = $ConfirmPublish.IsPresent
    allow_dirty = $AllowDirty.IsPresent
    continue_on_error = $ContinueOnError.IsPresent
    skip_token_check = $SkipTokenCheck.IsPresent
    skip_clean_tree_check = $SkipCleanTreeCheck.IsPresent
    retry_count = $RetryCount
    retry_delay_seconds = $RetryDelaySeconds
    next_wave_summary_path = $resolvedNextWaveSummary
    all_waves_complete = $allComplete
    next_publish_wave = $nextWave
    next_publish_crates = $nextCrates
    skipped = $skipped
    execute_exit_code = $executeExitCode
    execute_summary_path = $executeSummaryPath
}

$summary | ConvertTo-Json -Depth 8 | Set-Content -Path $summaryOutPath -Encoding utf8
Write-Log -Path $evidencePath -Message "Summary JSON: $summaryOutPath"

Write-Host "Evidence: $evidencePath"
Write-Host "Summary:  $summaryOutPath"

if (-not $skipped -and $executeExitCode -ne 0) {
    exit $executeExitCode
}
