param(
    [string]$EvidenceDir = "",
    [ValidateSet("heuristic", "default", "only-explicit")]
    [string]$SemverFeatureMode = "only-explicit",
    [string[]]$SemverPackages = @(
        "ranvier-core",
        "ranvier-runtime",
        "ranvier-http",
        "ranvier-std",
        "ranvier-macros",
        "ranvier",
        "ranvier-auth",
        "ranvier-guard",
        "ranvier-openapi",
        "ranvier-observe",
        "ranvier-inspector"
    ),
    [switch]$InstallSemverIfMissing,
    [switch]$FailOnStepFailure,
    [switch]$SkipSchematic,
    [switch]$SkipPersistence,
    [switch]$SkipSemver
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ranvierRoot = (Resolve-Path (Join-Path $scriptDir "..")).Path
$workspaceRoot = (Resolve-Path (Join-Path $ranvierRoot "..")).Path

if ([string]::IsNullOrWhiteSpace($EvidenceDir)) {
    $EvidenceDir = Join-Path $workspaceRoot "docs/05_dev_plans/evidence"
}
New-Item -ItemType Directory -Path $EvidenceDir -Force | Out-Null

$stamp = Get-Date -Format "yyyyMMdd_HHmmss"
$summaryPath = Join-Path $EvidenceDir ("m186_limitation_baseline_" + $stamp + ".json")
$bundleLog = Join-Path $EvidenceDir ("m186_limitation_baseline_" + $stamp + ".log")

function Write-BundleLog {
    param([string]$Message)
    $line = "[{0}] {1}" -f (Get-Date -Format "HH:mm:ss"), $Message
    $line | Tee-Object -FilePath $bundleLog -Append | Out-Host
}

function Invoke-Step {
    param(
        [string]$Name,
        [string]$Command,
        [scriptblock]$Action
    )

    Write-BundleLog ""
    Write-BundleLog "[step] $Name"
    Write-BundleLog "[command] $Command"

    $status = "pass"
    $errorText = $null
    $startedAt = (Get-Date).ToString("o")
    try {
        Push-Location $ranvierRoot
        try {
            & $Action
        } finally {
            Pop-Location
        }
    } catch {
        $status = "fail"
        $errorText = $_.Exception.Message
        Write-BundleLog "[error] $errorText"
    }
    $finishedAt = (Get-Date).ToString("o")

    return [ordered]@{
        name = $Name
        command = $Command
        status = $status
        started_at = $startedAt
        finished_at = $finishedAt
        error = $errorText
    }
}

$results = New-Object System.Collections.Generic.List[object]

try {
    Write-BundleLog "=== M186 Limitation Baseline ==="
    Write-BundleLog "workspace: $workspaceRoot"
    Write-BundleLog "ranvier: $ranvierRoot"
    Write-BundleLog "evidence_dir: $EvidenceDir"

    if (-not $SkipSchematic) {
        $cmd = "pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/m131_schematic_diff_policy_smoke.ps1 -EvidenceDir $EvidenceDir"
        $results.Add((Invoke-Step -Name "schematic_diff_policy_smoke" -Command $cmd -Action {
                    & (Join-Path $scriptDir "m131_schematic_diff_policy_smoke.ps1") -EvidenceDir $EvidenceDir *>&1 | Tee-Object -FilePath $bundleLog -Append | Out-Host
                    if ($LASTEXITCODE -ne 0) { throw "m131 schematic diff/policy smoke script failed (exit=$LASTEXITCODE)" }
                }))
    }

    if (-not $SkipPersistence) {
        $cmd = "pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/persistence_recovery_smoke.ps1 -EvidenceDir $EvidenceDir"
        $results.Add((Invoke-Step -Name "persistence_recovery_smoke" -Command $cmd -Action {
                    & (Join-Path $scriptDir "persistence_recovery_smoke.ps1") -EvidenceDir $EvidenceDir *>&1 | Tee-Object -FilePath $bundleLog -Append | Out-Host
                    if ($LASTEXITCODE -ne 0) { throw "persistence recovery smoke script failed (exit=$LASTEXITCODE)" }
                }))
    }

    if (-not $SkipSemver) {
        $pkgText = $SemverPackages -join ","
        $installText = if ($InstallSemverIfMissing) { "-InstallIfMissing" } else { "" }
        $cmd = "pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/m133_semver_checks.ps1 -FeatureMode $SemverFeatureMode -Packages $pkgText $installText -EvidenceDir $EvidenceDir"
        $results.Add((Invoke-Step -Name "semver_checks" -Command $cmd -Action {
                    $args = @{
                        FeatureMode = $SemverFeatureMode
                        Packages = $SemverPackages
                        EvidenceDir = $EvidenceDir
                    }
                    if ($InstallSemverIfMissing) {
                        $args.InstallIfMissing = $true
                    }
                    & (Join-Path $scriptDir "m133_semver_checks.ps1") @args *>&1 | Tee-Object -FilePath $bundleLog -Append | Out-Host
                    if ($LASTEXITCODE -ne 0) { throw "m133 semver checks script failed (exit=$LASTEXITCODE)" }
                }))
    }

    $failed = @($results | Where-Object { $_.status -ne "pass" })
    $summary = [ordered]@{
        timestamp = $stamp
        generated_by = "scripts/m186_limitation_baseline.ps1"
        evidence_dir = $EvidenceDir
        failed = ($failed.Count -gt 0)
        results = $results
    }
    $summary | ConvertTo-Json -Depth 8 | Set-Content -Path $summaryPath -Encoding UTF8

    Write-BundleLog ""
    Write-BundleLog "summary_json: $summaryPath"
    Write-BundleLog "bundle_log: $bundleLog"

    if ($failed.Count -gt 0 -and $FailOnStepFailure) {
        throw "one or more M186 baseline steps failed"
    }
} catch {
    Write-BundleLog "[fatal] $($_.Exception.Message)"
    throw
}
