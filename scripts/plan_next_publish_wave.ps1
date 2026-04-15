param(
    [ValidateSet("m119", "m131", "all")]
    [string]$Profile = "all",
    [string]$TargetVersion,
    [string]$WaveSummaryPath,
    [string]$RegistrySummaryPath,
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
        [string]$Pattern
    )

    $latest = Get-ChildItem -Path $Directory -Filter $Pattern -File -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTimeUtc -Descending |
        Select-Object -First 1

    if ($null -eq $latest) {
        return $null
    }

    return $latest.FullName
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
    $latest = Get-LatestFile -Directory $EvidenceRoot -Pattern $pattern
    if ($null -eq $latest) {
        throw "No wave summary found for profile=$ProfileKey in $EvidenceRoot"
    }
    return $latest
}

function Resolve-RegistrySummaryPath {
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

    $pattern = "cratesio_version_snapshot_${ProfileKey}_${Target}_*.json"
    $latest = Get-LatestFile -Directory $EvidenceRoot -Pattern $pattern
    if ($null -eq $latest) {
        throw "No registry summary found for profile=$ProfileKey target=$Target in $EvidenceRoot"
    }
    return $latest
}

function To-StringArray {
    param([object]$Value)

    if ($null -eq $Value) {
        return [string[]]@()
    }

    if ($Value -is [string]) {
        return [string[]]@([string]$Value)
    }

    if ($Value -is [System.Array] -or $Value -is [System.Collections.IEnumerable]) {
        $items = New-Object System.Collections.Generic.List[string]
        foreach ($entry in $Value) {
            if ($null -eq $entry) {
                continue
            }
            $text = [string]$entry
            if (-not [string]::IsNullOrWhiteSpace($text)) {
                [void]$items.Add($text)
            }
        }
        return @($items)
    }

    return [string[]]@([string]$Value)
}

function New-StringSet {
    return New-Object "System.Collections.Generic.HashSet[string]"
}

$workspaceRoot = Get-ReleaseWorkspaceRoot -ScriptRoot $PSScriptRoot
$profileKey = $Profile.ToLowerInvariant()
$target = Resolve-TargetVersion -Requested $TargetVersion -WorkspaceRoot $workspaceRoot
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$EvidenceDir = Resolve-ReleaseEvidenceDir -Requested $EvidenceDir -WorkspaceRoot $workspaceRoot

New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
$evidencePath = Join-Path $EvidenceDir "publish_next_wave_gate_${profileKey}_${target}_${timestamp}.log"
$summaryOutPath = Join-Path $EvidenceDir "publish_next_wave_gate_${profileKey}_${target}_${timestamp}.json"

$resolvedWaveSummaryPath = Resolve-WaveSummaryPath -Requested $WaveSummaryPath -ProfileKey $profileKey -EvidenceRoot $EvidenceDir -WorkspaceRoot "$workspaceRoot"
$resolvedRegistrySummaryPath = Resolve-RegistrySummaryPath -Requested $RegistrySummaryPath -ProfileKey $profileKey -Target $target -EvidenceRoot $EvidenceDir -WorkspaceRoot "$workspaceRoot"

$waveSummary = Get-Content -Path $resolvedWaveSummaryPath -Raw | ConvertFrom-Json
$registrySummary = Get-Content -Path $resolvedRegistrySummaryPath -Raw | ConvertFrom-Json

Write-Log -Path $evidencePath -Message "Next-wave gate planning started (profile=$profileKey, target=$target)"
Write-Log -Path $evidencePath -Message "Wave summary: $resolvedWaveSummaryPath"
Write-Log -Path $evidencePath -Message "Registry summary: $resolvedRegistrySummaryPath"

$targetPresentSet = New-StringSet
foreach ($crate in (To-StringArray $registrySummary.target_present_crates)) {
    [void]$targetPresentSet.Add($crate)
}

$targetMissingSet = New-StringSet
foreach ($crate in (To-StringArray $registrySummary.target_missing_crates)) {
    [void]$targetMissingSet.Add($crate)
}

$notFoundSet = New-StringSet
foreach ($crate in (To-StringArray $registrySummary.not_found_crates)) {
    [void]$notFoundSet.Add($crate)
}

$waveEntries = @(
    @($waveSummary.waves) |
        Sort-Object { [int]$_.wave }
)

$incompleteWaves = New-Object System.Collections.Generic.List[int]
$analysis = New-Object System.Collections.Generic.List[object]
$nextWave = $null
$nextCrates = @()

foreach ($entry in $waveEntries) {
    $waveNo = [int]$entry.wave
    $crates = To-StringArray $entry.crates

    $present = New-Object System.Collections.Generic.List[string]
    $missing = New-Object System.Collections.Generic.List[string]
    $missingReasons = New-Object System.Collections.Generic.List[object]

    foreach ($crate in $crates) {
        if ($targetPresentSet.Contains($crate)) {
            [void]$present.Add($crate)
        } else {
            [void]$missing.Add($crate)

            $reason = "not-in-registry-snapshot"
            if ($notFoundSet.Contains($crate)) {
                $reason = "crate-not-found"
            } elseif ($targetMissingSet.Contains($crate)) {
                $reason = "target-version-missing"
            }

            $missingReasons.Add([ordered]@{
                crate = $crate
                reason = $reason
            })
        }
    }

    $isComplete = ($missing.Count -eq 0)
    $readyNow = (-not $isComplete) -and ($incompleteWaves.Count -eq 0)
    $blockedBy = @($incompleteWaves.ToArray())

    $status = "blocked"
    if ($isComplete) {
        $status = "complete"
    } elseif ($readyNow) {
        $status = "ready"
    }

    if ($readyNow -and $null -eq $nextWave) {
        $nextWave = $waveNo
        $nextCrates = @($missing.ToArray())
    }

    $analysis.Add([ordered]@{
        wave = $waveNo
        status = $status
        crates = $crates
        crates_target_present = @($present.ToArray())
        crates_target_missing = @($missing.ToArray())
        missing_reasons = @($missingReasons.ToArray())
        blocked_by_incomplete_waves = $blockedBy
    })

    if (-not $isComplete) {
        [void]$incompleteWaves.Add($waveNo)
    }
}

$allWavesComplete = ($incompleteWaves.Count -eq 0)
$nextCommands = @($nextCrates | ForEach-Object { "cargo publish -p $_" })

$summary = [ordered]@{
    timestamp = $timestamp
    profile = $profileKey
    target_version = $target
    wave_summary_path = $resolvedWaveSummaryPath
    registry_summary_path = $resolvedRegistrySummaryPath
    all_waves_complete = $allWavesComplete
    next_publish_wave = $nextWave
    next_publish_crates = $nextCrates
    next_publish_commands = $nextCommands
    incomplete_waves = @($incompleteWaves.ToArray())
    wave_analysis = $analysis
}

$summary | ConvertTo-Json -Depth 8 | Set-Content -Path $summaryOutPath -Encoding utf8

if ($allWavesComplete) {
    Write-Log -Path $evidencePath -Message "All waves already complete for target version."
} else {
    Write-Log -Path $evidencePath -Message "Next publish wave: $nextWave"
    Write-Log -Path $evidencePath -Message "Next publish crates: $($nextCrates -join ', ')"
}

Write-Log -Path $evidencePath -Message "Summary JSON: $summaryOutPath"
Write-Host "Evidence: $evidencePath"
Write-Host "Summary:  $summaryOutPath"
