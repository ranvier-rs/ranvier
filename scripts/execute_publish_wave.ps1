param(
    [ValidateSet("m119", "m131", "all")]
    [string]$Profile = "all",
    [int]$Wave = 1,
    [ValidateSet("dry-run", "publish")]
    [string]$Mode = "dry-run",
    [switch]$AllowDirty,
    [switch]$ContinueOnError,
    [ValidateRange(0, 20)]
    [int]$RetryCount = 0,
    [ValidateRange(1, 600)]
    [int]$RetryDelaySeconds = 20,
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

function Invoke-CargoPublish {
    param(
        [string]$Crate,
        [string]$ModeKey,
        [bool]$AllowDirtyFlag,
        [string]$ProfileKey,
        [int]$WaveIndex,
        [int]$Attempt,
        [string]$Timestamp,
        [string]$EvidenceRoot,
        [string]$AggregateLogPath
    )

    $sanitized = $Crate.Replace("-", "_")
    $attemptSuffix = if ($Attempt -gt 0) { "_attempt$Attempt" } else { "" }
    $crateLogPath = Join-Path $EvidenceRoot "publish_wave_execute_${ProfileKey}_w${WaveIndex}_${sanitized}_${Timestamp}${attemptSuffix}.log"

    $args = @("publish", "-p", $Crate)
    if ($ModeKey -eq "dry-run") {
        $args += "--dry-run"
    }
    if ($AllowDirtyFlag) {
        $args += "--allow-dirty"
    }

    $commandLine = "cargo $($args -join ' ')"
    Write-Log -Path $AggregateLogPath -Message "Running: $commandLine"

    $previousErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        $commandOutput = & cargo @args 2>&1
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }

    $outputLines = @($commandOutput | ForEach-Object { [string]$_ })
    Set-Content -Path $crateLogPath -Value $outputLines -Encoding utf8
    Add-Content -Path $AggregateLogPath -Value $outputLines -Encoding utf8

    $tail = @()
    if (Test-Path $crateLogPath) {
        $tail = @((Get-Content $crateLogPath -Tail 25 | ForEach-Object { [string]$_ }))
    }

    return [ordered]@{
        crate = $Crate
        attempt = $Attempt
        command = $commandLine
        success = ($exitCode -eq 0)
        exit_code = $exitCode
        log_path = $crateLogPath
        tail = $tail
    }
}

$workspaceRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$profileKey = $Profile.ToLowerInvariant()
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"

New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
$evidencePath = Join-Path $EvidenceDir "publish_wave_execute_${profileKey}_w${Wave}_${timestamp}.log"
$summaryOutPath = Join-Path $EvidenceDir "publish_wave_execute_${profileKey}_w${Wave}_${timestamp}.json"

$waveSummaryPath = Resolve-WaveSummaryPath -Requested $WaveSummaryPath -ProfileKey $profileKey -EvidenceRoot $EvidenceDir -WorkspaceRoot "$workspaceRoot"
$waveSummary = Get-Content -Path $waveSummaryPath -Raw | ConvertFrom-Json

Write-Log -Path $evidencePath -Message "Publish wave execution started (profile=$profileKey, wave=$Wave, mode=$Mode, allow_dirty=$($AllowDirty.IsPresent), retry_count=$RetryCount, retry_delay_seconds=$RetryDelaySeconds)"
Write-Log -Path $evidencePath -Message "Workspace root: $workspaceRoot"
Write-Log -Path $evidencePath -Message "Input wave summary: $waveSummaryPath"

$selectedWave = $null
foreach ($entry in @($waveSummary.waves)) {
    if ([int]$entry.wave -eq $Wave) {
        $selectedWave = $entry
        break
    }
}

if ($null -eq $selectedWave) {
    throw "Wave $Wave not found in $waveSummaryPath"
}

$selectedCrates = @(
    @($selectedWave.crates) |
        ForEach-Object { [string]$_ } |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
)

Write-Log -Path $evidencePath -Message "Selected crates: $($selectedCrates -join ', ')"

$results = New-Object System.Collections.Generic.List[object]
$stoppedEarly = $false

foreach ($crate in $selectedCrates) {
    $attempt = 0
    $attemptResults = New-Object System.Collections.Generic.List[object]

    while ($true) {
        $attemptResult = Invoke-CargoPublish -Crate $crate -ModeKey $Mode -AllowDirtyFlag:$AllowDirty.IsPresent -ProfileKey $profileKey -WaveIndex $Wave -Attempt $attempt -Timestamp $timestamp -EvidenceRoot $EvidenceDir -AggregateLogPath $evidencePath
        $attemptResults.Add($attemptResult)

        if ($attemptResult.success -or $attempt -ge $RetryCount) {
            break
        }

        $nextAttempt = $attempt + 1
        Write-Log -Path $evidencePath -Message "Retrying $crate in $RetryDelaySeconds seconds (attempt $nextAttempt/$RetryCount)..."
        Start-Sleep -Seconds $RetryDelaySeconds
        $attempt = $nextAttempt
    }

    $finalAttempt = $attemptResults[$attemptResults.Count - 1]
    $attemptSummaries = @(
        $attemptResults | ForEach-Object {
            [ordered]@{
                attempt = [int]$_.attempt
                success = [bool]$_.success
                exit_code = [int]$_.exit_code
                log_path = [string]$_.log_path
            }
        }
    )

    $result = [ordered]@{
        crate = [string]$finalAttempt.crate
        command = [string]$finalAttempt.command
        success = [bool]$finalAttempt.success
        exit_code = [int]$finalAttempt.exit_code
        log_path = [string]$finalAttempt.log_path
        tail = @($finalAttempt.tail | ForEach-Object { [string]$_ })
        final_attempt = [int]$finalAttempt.attempt
        retry_attempts = [int]($attemptResults.Count - 1)
        attempts = $attemptSummaries
    }
    $results.Add($result)

    if ($result.success) {
        Write-Log -Path $evidencePath -Message "PASS: $crate (attempts=$($result.retry_attempts + 1))"
    } else {
        Write-Log -Path $evidencePath -Message "FAIL: $crate (exit=$($result.exit_code), attempts=$($result.retry_attempts + 1))"
        if (-not $ContinueOnError.IsPresent) {
            $stoppedEarly = $true
            Write-Log -Path $evidencePath -Message "Stopping after first failure (use -ContinueOnError to continue)."
            break
        }
    }
}

$failed = @($results | Where-Object { -not $_.success })
$summary = [ordered]@{
    timestamp = $timestamp
    profile = $profileKey
    wave = $Wave
    mode = $Mode
    allow_dirty = $AllowDirty.IsPresent
    continue_on_error = $ContinueOnError.IsPresent
    retry_count = $RetryCount
    retry_delay_seconds = $RetryDelaySeconds
    input_wave_summary = $waveSummaryPath
    selected_wave_crates = $selectedCrates
    total_selected = $selectedCrates.Count
    total_executed = $results.Count
    passed = ($results.Count - $failed.Count)
    failed = $failed.Count
    failed_crates = @($failed | ForEach-Object { [string]$_.crate })
    stopped_early = $stoppedEarly
    results = $results
}

$summary | ConvertTo-Json -Depth 8 | Set-Content -Path $summaryOutPath -Encoding utf8
Write-Log -Path $evidencePath -Message "Summary JSON: $summaryOutPath"

Write-Host "Evidence: $evidencePath"
Write-Host "Summary:  $summaryOutPath"

if ($failed.Count -gt 0) {
    exit 1
}
