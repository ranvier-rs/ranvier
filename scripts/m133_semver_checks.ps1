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
    [ValidateSet("heuristic", "default", "only-explicit")]
    [string]$FeatureMode = "only-explicit",
    [string]$EvidenceDir = "..\docs\05_dev_plans\evidence",
    [switch]$InstallIfMissing
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$logPath = Join-Path $EvidenceDir "m133_semver_checks_$timestamp.log"
$jsonPath = Join-Path $EvidenceDir "m133_semver_checks_$timestamp.json"
New-Item -ItemType Directory -Path $EvidenceDir -Force | Out-Null
Set-Content -Path $logPath -Value "" -Encoding UTF8

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

function Resolve-FeatureArgs {
    switch ($FeatureMode) {
        "default" { return @("--default-features") }
        "only-explicit" { return @("--only-explicit-features") }
        default { return @() }
    }
}

function Write-Log {
    param([string]$Message)
    $Message | Tee-Object -FilePath $logPath -Append | Out-Host
}

$rows = New-Object System.Collections.Generic.List[object]
$failed = $false

try {
    Write-Log "=== M133 cargo-semver-checks Baseline ==="
    Write-Log "Timestamp: $timestamp"
    Write-Log "Workspace: $(Get-Location)"
    Write-Log "Feature mode: $FeatureMode"

    Ensure-SemverTool
    $featureArgs = Resolve-FeatureArgs

    foreach ($pkg in $Packages) {
        Write-Log ""
        Write-Log "[check-release] $pkg"

        $args = @("semver-checks", "check-release", "-p", $pkg) + $featureArgs
        Write-Log "[command] cargo $($args -join ' ')"
        $tmpOutput = Join-Path $env:TEMP ("m133_semver_" + $pkg.Replace("-", "_") + "_" + $timestamp + ".log")
        $cmdLine = "cargo $($args -join ' ') > `"$tmpOutput`" 2>&1"
        cmd /c $cmdLine | Out-Null
        $exitCode = $LASTEXITCODE
        $commandOutput = @()
        if (Test-Path $tmpOutput) {
            $commandOutput = Get-Content -Path $tmpOutput
            $commandOutput | Tee-Object -FilePath $logPath -Append | Out-Host
            Remove-Item -Path $tmpOutput -Force
        }
        $blockedByPolicy = $false
        if ($exitCode -ne 0) {
            $blockedByPolicy = ($commandOutput -join "`n") -match "os error 4551"
        }

        $result = if ($exitCode -eq 0) {
            "pass"
        } elseif ($blockedByPolicy) {
            "blocked_policy"
        } else {
            "fail"
        }

        $rows.Add([ordered]@{
                package = $pkg
                command = "cargo $($args -join ' ')"
                feature_mode = $FeatureMode
                exit_code = $exitCode
                result = $result
            })

        if ($exitCode -ne 0) {
            $failed = $true
            Write-Log "[fail] $pkg (exit=$exitCode, result=$result)"
        } else {
            Write-Log "[pass] $pkg"
        }
    }

    $summary = [ordered]@{
        timestamp = $timestamp
        generated_by = "scripts/m133_semver_checks.ps1"
        feature_mode = $FeatureMode
        package_count = $rows.Count
        failed = $failed
        results = $rows
    }
    $summary | ConvertTo-Json -Depth 6 | Set-Content -Path $jsonPath -Encoding UTF8

    Write-Log ""
    Write-Log "Result JSON: $jsonPath"
    Write-Log "Log: $logPath"

    if ($failed) {
        throw "One or more semver checks failed"
    }
} finally {
    # log file is written incrementally via Tee-Object
}
