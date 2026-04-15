param(
    [ValidateSet("m119", "m131", "all")]
    [string]$Profile = "all",
    [switch]$NoAllowDirty,
    [string]$EvidenceDir = "..\docs\05_dev_plans\evidence"
)

$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "release_common.ps1")

$workspaceRoot = Get-ReleaseWorkspaceRoot -ScriptRoot $PSScriptRoot
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$allowDirty = -not $NoAllowDirty
$profileKey = $Profile.ToLowerInvariant()
$EvidenceDir = Resolve-ReleaseEvidenceDir -Requested $EvidenceDir -WorkspaceRoot $workspaceRoot

New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
$evidencePath = Join-Path $EvidenceDir "publish_dry_run_preflight_${profileKey}_${timestamp}.log"
$summaryPath = Join-Path $EvidenceDir "publish_dry_run_preflight_${profileKey}_${timestamp}.json"

function Write-Log {
    param([string]$Message)
    $line = "[{0}] {1}" -f (Get-Date -Format "HH:mm:ss"), $Message
    Write-Host $line
    Add-Content -Path $evidencePath -Value $line -Encoding utf8
}

function Resolve-CrateSet {
    param(
        [string]$Key,
        [string]$WorkspaceRoot
    )

    return Resolve-ReleaseCrateSet -ProfileKey $Key -WorkspaceRoot $WorkspaceRoot
}

function Resolve-PublishPlan {
    param(
        [string[]]$Crates,
        [string]$WorkspaceRoot
    )

    $crateSet = New-Object "System.Collections.Generic.HashSet[string]"
    foreach ($crate in $Crates) {
        [void]$crateSet.Add($crate)
    }

    $manifestPath = Join-Path $WorkspaceRoot "Cargo.toml"
    $metadataRaw = & cargo metadata --format-version 1 --no-deps --offline --manifest-path $manifestPath 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to resolve cargo metadata for publish plan."
    }
    $metadata = $metadataRaw | ConvertFrom-Json

    $packagesByName = @{}
    foreach ($pkg in $metadata.packages) {
        $packagesByName[$pkg.name] = $pkg
    }

    $dependencyMap = @{}
    $outOfScopeDependencyMap = @{}
    $adjacency = @{}
    $indegree = @{}

    foreach ($crate in $Crates) {
        $dependencyMap[$crate] = @()
        $outOfScopeDependencyMap[$crate] = @()
        $adjacency[$crate] = @()
        $indegree[$crate] = 0
    }

    foreach ($crate in $Crates) {
        if (-not $packagesByName.ContainsKey($crate)) {
            continue
        }

        $deps = New-Object System.Collections.Generic.List[string]
        $outOfScopeDeps = New-Object System.Collections.Generic.List[string]
        foreach ($dep in $packagesByName[$crate].dependencies) {
            if ($crateSet.Contains($dep.name)) {
                $deps.Add($dep.name)
            } elseif ($dep.name -like "ranvier-*" -and $packagesByName.ContainsKey($dep.name)) {
                $outOfScopeDeps.Add($dep.name)
            }
        }
        $uniqueDeps = @($deps | Sort-Object -Unique)
        $uniqueOutOfScopeDeps = @($outOfScopeDeps | Sort-Object -Unique)
        $dependencyMap[$crate] = $uniqueDeps
        $outOfScopeDependencyMap[$crate] = $uniqueOutOfScopeDeps

        foreach ($depName in $uniqueDeps) {
            $adjacency[$depName] = @($adjacency[$depName] + $crate)
            $indegree[$crate] = [int]$indegree[$crate] + 1
        }
    }

    $queue = New-Object "System.Collections.Generic.SortedSet[string]"
    foreach ($crate in ($Crates | Sort-Object -Unique)) {
        if ([int]$indegree[$crate] -eq 0) {
            [void]$queue.Add($crate)
        }
    }

    $order = New-Object System.Collections.Generic.List[string]
    while ($queue.Count -gt 0) {
        $next = $queue.Min
        [void]$queue.Remove($next)
        $order.Add($next)

        foreach ($dependent in @($adjacency[$next] | Sort-Object -Unique)) {
            $indegree[$dependent] = [int]$indegree[$dependent] - 1
            if ([int]$indegree[$dependent] -eq 0) {
                [void]$queue.Add($dependent)
            }
        }
    }

    if ($order.Count -ne $Crates.Count) {
        $remaining = @($Crates | Where-Object { -not $order.Contains($_) } | Sort-Object -Unique)
        foreach ($crate in $remaining) {
            $order.Add($crate)
        }
    }

    $edges = New-Object System.Collections.Generic.List[string]
    $outOfScopeEdges = New-Object System.Collections.Generic.List[string]
    foreach ($crate in $Crates) {
        foreach ($depName in @($dependencyMap[$crate])) {
            $edges.Add("$depName -> $crate")
        }
        foreach ($depName in @($outOfScopeDependencyMap[$crate])) {
            $outOfScopeEdges.Add("$depName -> $crate")
        }
    }

    return @{
        publish_order = @($order)
        dependency_map = $dependencyMap
        dependency_edges = @($edges | Sort-Object -Unique)
        out_of_scope_dependency_map = $outOfScopeDependencyMap
        out_of_scope_dependency_edges = @($outOfScopeEdges | Sort-Object -Unique)
    }
}

function Invoke-PublishDryRun {
    param(
        [string]$Crate,
        [bool]$AllowDirty,
        [string]$WorkspaceRoot,
        [string]$EvidenceDir,
        [string]$Timestamp
    )

    $sanitized = $Crate.Replace("-", "_")
    $crateLogPath = Join-Path $EvidenceDir "publish_dry_run_preflight_${sanitized}_${Timestamp}.log"

    $manifestPath = Join-Path $WorkspaceRoot "Cargo.toml"
    $args = @("publish", "--manifest-path", $manifestPath, "-p", $Crate, "--dry-run")
    if ($AllowDirty) {
        $args += "--allow-dirty"
    }

    $commandLine = "cargo $($args -join ' ')"
    Write-Log "Running: $commandLine"

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
    Add-Content -Path $evidencePath -Value $outputLines -Encoding utf8

    $tail = @()
    if (Test-Path $crateLogPath) {
        $tail = @((Get-Content $crateLogPath -Tail 25 | ForEach-Object { [string]$_ }))
    }

    return @{
        crate = $Crate
        success = ($exitCode -eq 0)
        exit_code = $exitCode
        command = $commandLine
        log_path = $crateLogPath
        tail = $tail
    }
}

$crates = Resolve-CrateSet -Key $profileKey -WorkspaceRoot $workspaceRoot
Write-Log "Publish dry-run preflight started (profile=$profileKey, allow_dirty=$allowDirty)"
Write-Log "Workspace root: $workspaceRoot"
Write-Log "Crates: $($crates -join ', ')"
Write-Log "Resolving publish order plan from workspace metadata..."
try {
    $publishPlan = Resolve-PublishPlan -Crates $crates -WorkspaceRoot "$workspaceRoot"
    Write-Log "Suggested publish order: $($publishPlan.publish_order -join ', ')"
    if (@($publishPlan.out_of_scope_dependency_edges).Count -gt 0) {
        Write-Log "Detected profile-external internal dependencies:"
        foreach ($edge in @($publishPlan.out_of_scope_dependency_edges)) {
            Write-Log "  - $edge"
        }
    }
} catch {
    Write-Log "WARN: publish order planning failed; using profile order. reason=$($_.Exception.Message)"
    $publishPlan = @{
        publish_order = @($crates)
        dependency_map = @{}
        dependency_edges = @()
        out_of_scope_dependency_map = @{}
        out_of_scope_dependency_edges = @()
    }
}

$results = New-Object System.Collections.Generic.List[object]
foreach ($crate in @($publishPlan.publish_order)) {
    $result = Invoke-PublishDryRun -Crate $crate -AllowDirty:$allowDirty -WorkspaceRoot $workspaceRoot -EvidenceDir $EvidenceDir -Timestamp $timestamp
    $results.Add($result)
    if ($result.success) {
        Write-Log "PASS: $crate"
    } else {
        Write-Log "FAIL: $crate (exit=$($result.exit_code))"
    }
}

$failed = @($results | Where-Object { -not $_.success })
$summary = [ordered]@{
    timestamp = $timestamp
    profile = $profileKey
    allow_dirty = $allowDirty
    workspace_root = "$workspaceRoot"
    total = $results.Count
    passed = ($results.Count - $failed.Count)
    failed = $failed.Count
    failed_crates = @($failed | ForEach-Object { $_.crate })
    suggested_publish_order = @($publishPlan.publish_order)
    dependency_edges = @($publishPlan.dependency_edges)
    out_of_scope_dependency_edges = @($publishPlan.out_of_scope_dependency_edges)
    results = $results
}

$summary | ConvertTo-Json -Depth 8 | Set-Content -Path $summaryPath -Encoding utf8
Write-Log "Summary JSON: $summaryPath"

if ($failed.Count -gt 0) {
    Write-Log "Preflight failed for crates: $($summary.failed_crates -join ', ')"
    Write-Host "Evidence: $evidencePath"
    Write-Host "Summary:  $summaryPath"
    exit 1
}

Write-Log "Preflight succeeded for all crates"
Write-Host "Evidence: $evidencePath"
Write-Host "Summary:  $summaryPath"
