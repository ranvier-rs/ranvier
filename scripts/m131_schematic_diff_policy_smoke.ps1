param(
    [string]$EvidenceDir = "..\docs\05_dev_plans\evidence"
)

$ErrorActionPreference = "Stop"

$workspaceRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$evidencePath = Join-Path $EvidenceDir "m131_schematic_diff_policy_smoke_$timestamp.log"
New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null

function Write-Log {
    param([string]$Message)
    $line = "[{0}] {1}" -f (Get-Date -Format "HH:mm:ss"), $Message
    $line | Tee-Object -FilePath $evidencePath -Append
}

$diffReportPath = Join-Path $EvidenceDir "m131_schematic_diff_report_$timestamp.json"
$policyReportPath = Join-Path $EvidenceDir "m131_schematic_policy_report_$timestamp.json"
$diffCliLog = Join-Path $EvidenceDir "m131_schematic_diff_cli_$timestamp.log"
$policyCliLog = Join-Path $EvidenceDir "m131_schematic_policy_cli_$timestamp.log"

$gitRoot = $workspaceRoot.Path
$tempWorktree = Join-Path $env:TEMP ("ranvier-diff-policy-" + $timestamp)
$tempCommit = $null

try {
    Write-Log "Creating temporary worktree at $tempWorktree"
    git -C $gitRoot worktree add --detach $tempWorktree HEAD *>&1 | Tee-Object -FilePath $evidencePath -Append | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "failed to create temporary worktree"
    }

    $target = Join-Path $tempWorktree "examples\basic-schematic\src\main.rs"
    if (-not (Test-Path $target)) {
        throw "fixture target not found: $target"
    }

    Write-Log "Injecting deterministic node-label change in temporary worktree"
    $content = Get-Content $target -Raw
    if ($content -notmatch "ProcessData") {
        throw "fixture source does not contain ProcessData label"
    }
    $content = $content.Replace("ProcessData", "ProcessPayload")
    Set-Content -Path $target -Value $content

    git -C $tempWorktree add examples/basic-schematic/src/main.rs
    git -C $tempWorktree -c user.name='codex' -c user.email='codex@example.com' commit -m "temp: rename node for schematic diff policy smoke" *>&1 | Tee-Object -FilePath $evidencePath -Append | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "failed to create temporary fixture commit"
    }
    $tempCommit = (git -C $tempWorktree rev-parse HEAD).Trim()
    Write-Log "Temporary fixture commit created: $tempCommit"

    Write-Log "Running schematic diff command"
    cargo run --manifest-path ..\cli\Cargo.toml -- schematic diff `
        --example basic-schematic `
        --base HEAD `
        --head $tempCommit `
        --workspace . `
        --output $diffReportPath `
        *>&1 | Tee-Object -FilePath $diffCliLog | Tee-Object -FilePath $evidencePath -Append | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "schematic diff command failed"
    }

    if (-not (Test-Path $diffReportPath)) {
        throw "diff report file was not generated: $diffReportPath"
    }
    $diffJson = Get-Content $diffReportPath -Raw | ConvertFrom-Json
    if (-not $diffJson.summary.has_changes) {
        throw "diff summary indicates no changes for fixture commit"
    }
    if ($diffJson.summary.added_nodes -lt 1 -or $diffJson.summary.removed_nodes -lt 1) {
        throw "diff report did not capture expected node-level change"
    }
    Write-Log "Schematic diff report verified"

    Write-Log "Running schematic policy check command (expected failure)"
    $policyFailedAsExpected = $false
    cargo run --manifest-path ..\cli\Cargo.toml -- schematic policy check `
        --example basic-schematic `
        --base HEAD `
        --head $tempCommit `
        --workspace . `
        --max-added-nodes 0 `
        --output $policyReportPath `
        *>&1 | Tee-Object -FilePath $policyCliLog | Tee-Object -FilePath $evidencePath -Append | Out-Null
    if ($LASTEXITCODE -ne 0) {
        $policyFailedAsExpected = $true
    }
    if (-not $policyFailedAsExpected) {
        throw "policy check unexpectedly passed"
    }

    if (-not (Test-Path $policyReportPath)) {
        throw "policy report file was not generated: $policyReportPath"
    }
    $policyJson = Get-Content $policyReportPath -Raw | ConvertFrom-Json
    if ($null -eq $policyJson.violations -or $policyJson.violations.Count -eq 0) {
        throw "policy report does not include violation details"
    }
    Write-Log "Schematic policy check failure path verified"

    Write-Log "Schematic diff/policy smoke succeeded"
    Write-Host "Evidence: $evidencePath"
} finally {
    if (Test-Path $tempWorktree) {
        git -C $gitRoot worktree remove --force $tempWorktree *>&1 | Tee-Object -FilePath $evidencePath -Append | Out-Null
    }
}
