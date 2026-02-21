param(
    [ValidateSet("m119", "m131", "all")]
    [string]$Profile = "all",
    [switch]$NoAllowDirty,
    [string]$EvidenceDir = "..\docs\05_dev_plans\evidence"
)

$ErrorActionPreference = "Stop"

$workspaceRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$allowDirty = -not $NoAllowDirty
$profileKey = $Profile.ToLowerInvariant()

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
    param([string]$Key)

    $m119 = @(
        "ranvier-core",
        "ranvier-runtime",
        "ranvier-http",
        "ranvier-std",
        "ranvier-macros",
        "ranvier"
    )
    $m131 = @(
        "ranvier-observe",
        "ranvier-inspector",
        "ranvier-runtime",
        "ranvier-auth",
        "ranvier-guard",
        "ranvier-http",
        "ranvier-openapi",
        "ranvier"
    )

    switch ($Key) {
        "m119" { return $m119 }
        "m131" { return $m131 }
        "all" {
            $ordered = New-Object System.Collections.Generic.List[string]
            foreach ($name in ($m119 + $m131)) {
                if (-not $ordered.Contains($name)) {
                    $ordered.Add($name)
                }
            }
            return $ordered
        }
        default {
            throw "Unknown profile: $Key"
        }
    }
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
    $adjacency = @{}
    $indegree = @{}

    foreach ($crate in $Crates) {
        $dependencyMap[$crate] = @()
        $adjacency[$crate] = @()
        $indegree[$crate] = 0
    }

    foreach ($crate in $Crates) {
        if (-not $packagesByName.ContainsKey($crate)) {
            continue
        }

        $deps = New-Object System.Collections.Generic.List[string]
        foreach ($dep in $packagesByName[$crate].dependencies) {
            if ($crateSet.Contains($dep.name)) {
                $deps.Add($dep.name)
            }
        }
        $uniqueDeps = @($deps | Sort-Object -Unique)
        $dependencyMap[$crate] = $uniqueDeps

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
    foreach ($crate in $Crates) {
        foreach ($depName in @($dependencyMap[$crate])) {
            $edges.Add("$depName -> $crate")
        }
    }

    return @{
        publish_order = @($order)
        dependency_map = $dependencyMap
        dependency_edges = @($edges | Sort-Object -Unique)
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

    $args = @("publish", "-p", $Crate, "--dry-run")
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

    $outputLines = @($commandOutput | ForEach-Object { $_.ToString() })
    Set-Content -Path $crateLogPath -Value $outputLines -Encoding utf8
    Add-Content -Path $evidencePath -Value $outputLines -Encoding utf8

    $tail = @()
    if (Test-Path $crateLogPath) {
        $tail = Get-Content $crateLogPath -Tail 25
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

$crates = Resolve-CrateSet -Key $profileKey
Write-Log "Publish dry-run preflight started (profile=$profileKey, allow_dirty=$allowDirty)"
Write-Log "Workspace root: $workspaceRoot"
Write-Log "Crates: $($crates -join ', ')"
Write-Log "Resolving publish order plan from workspace metadata..."
try {
    $publishPlan = Resolve-PublishPlan -Crates $crates -WorkspaceRoot "$workspaceRoot"
    Write-Log "Suggested publish order: $($publishPlan.publish_order -join ', ')"
} catch {
    Write-Log "WARN: publish order planning failed; using profile order. reason=$($_.Exception.Message)"
    $publishPlan = @{
        publish_order = @($crates)
        dependency_map = @{}
        dependency_edges = @()
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
