param(
    [string[]]$Packages = @(
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
    [string]$EvidenceDir = "..\docs\05_dev_plans\evidence",
    [switch]$InstallIfMissing
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$logPath = Join-Path $EvidenceDir "m133_semver_checks_$timestamp.log"
$jsonPath = Join-Path $EvidenceDir "m133_semver_checks_$timestamp.json"
New-Item -ItemType Directory -Path $EvidenceDir -Force | Out-Null

function Ensure-SemverTool {
    $cargoList = & cargo --list
    if ($LASTEXITCODE -ne 0) {
        throw "failed to query cargo command list"
    }

    if (($cargoList -join "`n") -match "(?m)^\s*semver-checks\s") {
        return
    }

    if (-not $InstallIfMissing) {
        throw "cargo-semver-checks is not installed. Install with: cargo install cargo-semver-checks --locked"
    }

    Write-Host "[install] cargo install cargo-semver-checks --locked"
    & cargo install cargo-semver-checks --locked
    if ($LASTEXITCODE -ne 0) {
        throw "cargo install cargo-semver-checks failed"
    }

    & cargo semver-checks --version
    if ($LASTEXITCODE -ne 0) {
        throw "cargo-semver-checks installation verification failed"
    }
}

$rows = New-Object System.Collections.Generic.List[object]
$failed = $false

Start-Transcript -Path $logPath | Out-Null

try {
    Write-Host "=== M133 cargo-semver-checks Baseline ==="
    Write-Host "Timestamp: $timestamp"
    Write-Host "Workspace: $(Get-Location)"

    Ensure-SemverTool

    foreach ($pkg in $Packages) {
        Write-Host ""
        Write-Host "[check-release] $pkg"

        & cargo semver-checks check-release -p $pkg
        $exitCode = $LASTEXITCODE

        $rows.Add([ordered]@{
                package = $pkg
                command = "cargo semver-checks check-release -p $pkg"
                exit_code = $exitCode
                result = $(if ($exitCode -eq 0) { "pass" } else { "fail" })
            })

        if ($exitCode -ne 0) {
            $failed = $true
            Write-Host "[fail] $pkg (exit=$exitCode)"
        } else {
            Write-Host "[pass] $pkg"
        }
    }

    $summary = [ordered]@{
        timestamp = $timestamp
        generated_by = "scripts/m133_semver_checks.ps1"
        package_count = $rows.Count
        failed = $failed
        results = $rows
    }
    $summary | ConvertTo-Json -Depth 6 | Set-Content -Path $jsonPath -Encoding UTF8

    Write-Host ""
    Write-Host "Result JSON: $jsonPath"
    Write-Host "Log: $logPath"

    if ($failed) {
        throw "One or more semver checks failed"
    }
} finally {
    Stop-Transcript | Out-Null
}
