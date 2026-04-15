param(
    [ValidateSet("m119", "m131", "all")]
    [string]$Profile = "all",
    [string]$SummaryPath,
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

function Resolve-SummaryPath {
    param(
        [string]$Requested,
        [string]$ProfileKey,
        [string]$EvidenceRoot,
        [string]$WorkspaceRoot
    )

    if (-not [string]::IsNullOrWhiteSpace($Requested)) {
        return Resolve-InputPath -Value $Requested -BasePath $WorkspaceRoot
    }

    $pattern = "publish_dry_run_preflight_${ProfileKey}_*.json"
    $latest = Get-ChildItem -Path $EvidenceRoot -Filter $pattern -File |
        Sort-Object LastWriteTimeUtc -Descending |
        Select-Object -First 1

    if ($null -eq $latest) {
        throw "No summary JSON found for profile=$ProfileKey under $EvidenceRoot"
    }

    return $latest.FullName
}

function New-StringSet {
    return New-Object "System.Collections.Generic.HashSet[string]"
}

function Add-DependencyEdge {
    param(
        [string]$EdgeText,
        [hashtable]$DependencyMap
    )

    if ($EdgeText -notmatch '^\s*([^\s]+)\s*->\s*([^\s]+)\s*$') {
        return
    }

    $dep = $matches[1]
    $target = $matches[2]

    if (-not $DependencyMap.ContainsKey($target)) {
        $DependencyMap[$target] = New-StringSet
    }
    [void]$DependencyMap[$target].Add($dep)
}

function Parse-MissingDependencies {
    param([string[]]$Lines)

    $items = New-Object System.Collections.Generic.List[object]
    $seen = New-StringSet

    foreach ($line in $Lines) {
        if ($line -match 'no matching package named `([^`]+)` found') {
            $dep = $matches[1]
            if (-not $seen.Contains($dep)) {
                [void]$seen.Add($dep)
                $items.Add([ordered]@{
                    dependency = $dep
                    requirement = $null
                    kind = "missing-package"
                })
            }
        } elseif ($line -match 'failed to select a version for the requirement `([^`=\s]+)\s*=\s*"([^"]+)"`') {
            $dep = $matches[1]
            $req = $matches[2]
            $key = "$dep@$req"
            if (-not $seen.Contains($key)) {
                [void]$seen.Add($key)
                $items.Add([ordered]@{
                    dependency = $dep
                    requirement = $req
                    kind = "version-requirement"
                })
            }
        }
    }

    return ,$items.ToArray()
}

function Order-ByReference {
    param(
        [string[]]$Items,
        [string[]]$ReferenceOrder
    )

    $ordered = New-Object System.Collections.Generic.List[string]
    foreach ($item in $ReferenceOrder) {
        if ($Items -contains $item) {
            [void]$ordered.Add($item)
        }
    }
    foreach ($item in $Items) {
        if (-not $ordered.Contains($item)) {
            [void]$ordered.Add($item)
        }
    }
    return ,$ordered.ToArray()
}

$workspaceRoot = Get-ReleaseWorkspaceRoot -ScriptRoot $PSScriptRoot
$profileKey = $Profile.ToLowerInvariant()
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$EvidenceDir = Resolve-ReleaseEvidenceDir -Requested $EvidenceDir -WorkspaceRoot $workspaceRoot

New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
$evidencePath = Join-Path $EvidenceDir "publish_wave_plan_${profileKey}_${timestamp}.log"
$summaryOutPath = Join-Path $EvidenceDir "publish_wave_plan_${profileKey}_${timestamp}.json"

$summaryInputPath = Resolve-SummaryPath -Requested $SummaryPath -ProfileKey $profileKey -EvidenceRoot $EvidenceDir -WorkspaceRoot "$workspaceRoot"
$summary = Get-Content -Path $summaryInputPath -Raw | ConvertFrom-Json

Write-Log -Path $evidencePath -Message "Publish wave planning started (profile=$profileKey)"
Write-Log -Path $evidencePath -Message "Workspace root: $workspaceRoot"
Write-Log -Path $evidencePath -Message "Input summary: $summaryInputPath"

$resultOrder = @($summary.suggested_publish_order)
if ($resultOrder.Count -eq 0) {
    $resultOrder = @($summary.results | ForEach-Object { [string]$_.crate })
}

$resultsByCrate = @{}
$allCratesSet = New-StringSet
foreach ($result in $summary.results) {
    $crate = [string]$result.crate
    $resultsByCrate[$crate] = $result
    [void]$allCratesSet.Add($crate)
}
$allCrates = Order-ByReference -Items @($allCratesSet | Sort-Object) -ReferenceOrder $resultOrder

$dependencyMap = @{}
foreach ($crate in $allCrates) {
    $dependencyMap[$crate] = New-StringSet
}
foreach ($edge in @($summary.dependency_edges)) {
    Add-DependencyEdge -EdgeText ([string]$edge) -DependencyMap $dependencyMap
}
foreach ($edge in @($summary.out_of_scope_dependency_edges)) {
    Add-DependencyEdge -EdgeText ([string]$edge) -DependencyMap $dependencyMap
}

$passSet = New-StringSet
$failedCrates = New-Object System.Collections.Generic.List[string]
$failureDetails = @{}

foreach ($crate in $allCrates) {
    $result = $resultsByCrate[$crate]
    if ($result.success) {
        [void]$passSet.Add($crate)
        continue
    }

    [void]$failedCrates.Add($crate)
    $logPathRaw = [string]$result.log_path
    $resolvedLogPath = Resolve-InputPath -Value $logPathRaw -BasePath "$workspaceRoot"
    $logLines = @((Get-Content -Path $resolvedLogPath) | ForEach-Object { [string]$_ })
    $missingDeps = Parse-MissingDependencies -Lines $logLines

    $failureDetails[$crate] = [ordered]@{
        log_path = $resolvedLogPath
        missing_dependencies = $missingDeps
        non_dependency_failure = ($missingDeps.Count -eq 0)
    }
}

$waves = New-Object System.Collections.Generic.List[object]
$wave1Crates = Order-ByReference -Items @($passSet | Sort-Object) -ReferenceOrder $resultOrder
$waves.Add([ordered]@{
    wave = 1
    crates = $wave1Crates
    note = "publishable-now from current preflight pass set"
})

$availableSet = New-StringSet
foreach ($crate in $wave1Crates) {
    [void]$availableSet.Add($crate)
}

$remainingSet = New-StringSet
foreach ($crate in $failedCrates) {
    [void]$remainingSet.Add($crate)
}

$waveIndex = 2
while ($true) {
    $ready = New-Object System.Collections.Generic.List[string]
    foreach ($crate in $resultOrder) {
        if (-not $remainingSet.Contains($crate)) {
            continue
        }

        $failure = $failureDetails[$crate]
        if ($failure.non_dependency_failure) {
            continue
        }

        $deps = @($dependencyMap[$crate] | ForEach-Object { [string]$_ })
        $internalDeps = @($deps | Where-Object { $_ -like "ranvier-*" -or $_ -eq "ranvier" })
        $unresolved = @($internalDeps | Where-Object { -not $availableSet.Contains($_) })
        if ($unresolved.Count -eq 0) {
            [void]$ready.Add($crate)
        }
    }

    if ($ready.Count -eq 0) {
        break
    }

    $readyOrdered = Order-ByReference -Items @($ready | ForEach-Object { [string]$_ }) -ReferenceOrder $resultOrder
    foreach ($crate in $readyOrdered) {
        [void]$availableSet.Add($crate)
        [void]$remainingSet.Remove($crate)
    }

    $waves.Add([ordered]@{
        wave = $waveIndex
        crates = $readyOrdered
        note = "unblocked after prior wave publish assumptions"
    })
    $waveIndex++
}

$blocked = New-Object System.Collections.Generic.List[object]
foreach ($crate in $resultOrder) {
    if (-not $remainingSet.Contains($crate)) {
        continue
    }

    $deps = @($dependencyMap[$crate] | Where-Object { $_ -like "ranvier-*" -or $_ -eq "ranvier" })
    $unresolved = @($deps | Where-Object { -not $availableSet.Contains($_) } | Sort-Object -Unique)
    $failure = $failureDetails[$crate]
    $missingDeps = @($failure.missing_dependencies)

    $blocked.Add([ordered]@{
        crate = $crate
        unresolved_internal_dependencies = $unresolved
        parsed_missing_dependencies = $missingDeps
        non_dependency_failure = [bool]$failure.non_dependency_failure
    })
}

$analysisByCrate = New-Object System.Collections.Generic.List[object]
foreach ($crate in $resultOrder) {
    $deps = @($dependencyMap[$crate] | Sort-Object -Unique)
    $internalDeps = @($deps | Where-Object { $_ -like "ranvier-*" -or $_ -eq "ranvier" })
    $externalInternalDeps = @($internalDeps | Where-Object { -not $allCratesSet.Contains($_) } | Sort-Object -Unique)
    $wave = $null
    foreach ($waveEntry in $waves) {
        if (@($waveEntry.crates) -contains $crate) {
            $wave = [int]$waveEntry.wave
            break
        }
    }
    $failure = $failureDetails[$crate]
    $analysisByCrate.Add([ordered]@{
        crate = $crate
        wave = $wave
        success = [bool]$resultsByCrate[$crate].success
        internal_dependencies = $internalDeps
        internal_dependencies_outside_profile = $externalInternalDeps
        parsed_missing_dependencies = if ($null -ne $failure) { @($failure.missing_dependencies) } else { @() }
        non_dependency_failure = if ($null -ne $failure) { [bool]$failure.non_dependency_failure } else { $false }
    })
}

$summaryOut = [ordered]@{
    timestamp = $timestamp
    profile = $profileKey
    input_summary = $summaryInputPath
    wave_count = $waves.Count
    waves = $waves
    blocked_count = $blocked.Count
    blocked = $blocked
    crate_analysis = $analysisByCrate
}

$summaryOut | ConvertTo-Json -Depth 8 | Set-Content -Path $summaryOutPath -Encoding utf8

Write-Log -Path $evidencePath -Message "Wave plan:"
foreach ($waveEntry in $waves) {
    $joined = @($waveEntry.crates) -join ", "
    Write-Log -Path $evidencePath -Message "  wave$($waveEntry.wave): $joined"
}

if ($blocked.Count -gt 0) {
    Write-Log -Path $evidencePath -Message "Blocked crates:"
    foreach ($item in $blocked) {
        $unresolved = @($item.unresolved_internal_dependencies) -join ", "
        if ([string]::IsNullOrWhiteSpace($unresolved)) {
            $unresolved = "(none)"
        }
        Write-Log -Path $evidencePath -Message "  - $($item.crate): unresolved=$unresolved non_dependency_failure=$($item.non_dependency_failure)"
    }
}

Write-Log -Path $evidencePath -Message "Summary JSON: $summaryOutPath"
Write-Host "Evidence: $evidencePath"
Write-Host "Summary:  $summaryOutPath"
