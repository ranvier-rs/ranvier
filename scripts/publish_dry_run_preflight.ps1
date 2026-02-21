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
    Add-Content -Path $evidencePath -Value $line
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

    Write-Log "Running: cargo $($args -join ' ')"
    & cargo @args 2>&1 | Tee-Object -FilePath $crateLogPath | Tee-Object -FilePath $evidencePath -Append | Out-Null
    $exitCode = $LASTEXITCODE

    $tail = @()
    if (Test-Path $crateLogPath) {
        $tail = Get-Content $crateLogPath -Tail 25
    }

    return @{
        crate = $Crate
        success = ($exitCode -eq 0)
        exit_code = $exitCode
        command = "cargo $($args -join ' ')"
        log_path = $crateLogPath
        tail = $tail
    }
}

$crates = Resolve-CrateSet -Key $profileKey
Write-Log "Publish dry-run preflight started (profile=$profileKey, allow_dirty=$allowDirty)"
Write-Log "Workspace root: $workspaceRoot"
Write-Log "Crates: $($crates -join ', ')"

$results = New-Object System.Collections.Generic.List[object]
foreach ($crate in $crates) {
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
    results = $results
}

$summary | ConvertTo-Json -Depth 8 | Set-Content -Path $summaryPath
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
