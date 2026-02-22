param(
    [ValidateSet("m119", "m131", "all")]
    [string]$Profile = "all",
    [ValidateSet("dry-run", "publish")]
    [string]$Mode = "dry-run",
    [int]$StartWave = 1,
    [int]$EndWave = 0,
    [switch]$AllowDirty,
    [switch]$ContinueOnError,
    [string]$WaveSummaryPath,
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

function Resolve-WaveSummaryPath {
    param(
        [string]$Requested,
        [string]$ProfileKey,
        [string]$EvidenceRoot,
        [string]$WorkspaceRoot
    )

    if (-not [string]::IsNullOrWhiteSpace($Requested)) {
        return Resolve-InputPath -Value $Requested -BasePath $WorkspaceRoot
    }

    $pattern = "publish_wave_plan_${ProfileKey}_*.json"
    $latest = Get-ChildItem -Path $EvidenceRoot -Filter $pattern -File |
        Sort-Object LastWriteTimeUtc -Descending |
        Select-Object -First 1

    if ($null -eq $latest) {
        throw "No wave summary JSON found for profile=$ProfileKey under $EvidenceRoot"
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

$workspaceRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$profileKey = $Profile.ToLowerInvariant()
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$psExe = Resolve-PowerShellExecutable

if ($StartWave -lt 1) {
    throw "StartWave must be >= 1"
}
if ($EndWave -ne 0 -and $EndWave -lt $StartWave) {
    throw "EndWave must be 0 (all) or >= StartWave"
}

New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
$evidencePath = Join-Path $EvidenceDir "publish_train_execute_${profileKey}_${Mode}_${timestamp}.log"
$summaryOutPath = Join-Path $EvidenceDir "publish_train_execute_${profileKey}_${Mode}_${timestamp}.json"

$waveSummaryPath = Resolve-WaveSummaryPath -Requested $WaveSummaryPath -ProfileKey $profileKey -EvidenceRoot $EvidenceDir -WorkspaceRoot "$workspaceRoot"
$waveSummary = Get-Content -Path $waveSummaryPath -Raw | ConvertFrom-Json

$waveNumbers = @(
    @($waveSummary.waves) |
        ForEach-Object { [int]$_.wave } |
        Sort-Object -Unique
)

if ($waveNumbers.Count -eq 0) {
    throw "No waves found in summary: $waveSummaryPath"
}

$selectedWaves = @($waveNumbers | Where-Object {
    $_ -ge $StartWave -and ($EndWave -eq 0 -or $_ -le $EndWave)
})

if ($selectedWaves.Count -eq 0) {
    throw "No waves selected for StartWave=$StartWave EndWave=$EndWave in summary: $waveSummaryPath"
}

Write-Log -Path $evidencePath -Message "Publish train execution started (profile=$profileKey, mode=$Mode, start_wave=$StartWave, end_wave=$EndWave, allow_dirty=$($AllowDirty.IsPresent))"
Write-Log -Path $evidencePath -Message "Workspace root: $workspaceRoot"
Write-Log -Path $evidencePath -Message "Input wave summary: $waveSummaryPath"
Write-Log -Path $evidencePath -Message "Selected waves: $($selectedWaves -join ', ')"

$executeScript = Join-Path $PSScriptRoot "execute_publish_wave.ps1"
$waveResults = New-Object System.Collections.Generic.List[object]
$stoppedEarly = $false

foreach ($wave in $selectedWaves) {
    $waveStart = [datetime]::UtcNow
    $args = @(
        "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $executeScript,
        "-Profile", $profileKey,
        "-Wave", "$wave",
        "-Mode", $Mode,
        "-WaveSummaryPath", $waveSummaryPath
    )
    if ($AllowDirty.IsPresent) {
        $args += "-AllowDirty"
    }
    if ($ContinueOnError.IsPresent) {
        $args += "-ContinueOnError"
    }

    Write-Log -Path $evidencePath -Message "Running wave $wave via execute_publish_wave.ps1..."
    & $psExe @args
    $exitCode = $LASTEXITCODE
    Write-Log -Path $evidencePath -Message "Wave $wave exit_code=$exitCode"

    $pattern = "publish_wave_execute_${profileKey}_w${wave}_*.json"
    $waveSummaryFile = Get-LatestFile -Directory $EvidenceDir -Pattern $pattern -SinceUtc $waveStart

    $waveSummaryPathOut = $null
    $passed = $null
    $failed = $null
    $failedCrates = @()
    if ($null -ne $waveSummaryFile) {
        $waveSummaryPathOut = $waveSummaryFile.FullName
        $waveExecutionSummary = Get-Content -Path $waveSummaryFile.FullName -Raw | ConvertFrom-Json
        $passed = [int]$waveExecutionSummary.passed
        $failed = [int]$waveExecutionSummary.failed
        $failedCrates = @($waveExecutionSummary.failed_crates | ForEach-Object { [string]$_ })
    }

    $waveResults.Add([ordered]@{
        wave = [int]$wave
        exit_code = [int]$exitCode
        summary_path = $waveSummaryPathOut
        passed = $passed
        failed = $failed
        failed_crates = $failedCrates
    })

    if ($exitCode -ne 0 -and -not $ContinueOnError.IsPresent) {
        $stoppedEarly = $true
        Write-Log -Path $evidencePath -Message "Stopping after first failed wave (use -ContinueOnError to continue)."
        break
    }
}

$failedWaves = @($waveResults | Where-Object { [int]$_.exit_code -ne 0 })
$summary = [ordered]@{
    timestamp = $timestamp
    profile = $profileKey
    mode = $Mode
    allow_dirty = $AllowDirty.IsPresent
    continue_on_error = $ContinueOnError.IsPresent
    start_wave = $StartWave
    end_wave = $EndWave
    input_wave_summary = $waveSummaryPath
    selected_waves = $selectedWaves
    waves_executed = $waveResults.Count
    failed_wave_count = $failedWaves.Count
    failed_waves = @($failedWaves | ForEach-Object { [int]$_.wave })
    stopped_early = $stoppedEarly
    wave_results = $waveResults
}

$summary | ConvertTo-Json -Depth 8 | Set-Content -Path $summaryOutPath -Encoding utf8
Write-Log -Path $evidencePath -Message "Summary JSON: $summaryOutPath"

Write-Host "Evidence: $evidencePath"
Write-Host "Summary:  $summaryOutPath"

if ($failedWaves.Count -gt 0) {
    exit 1
}
