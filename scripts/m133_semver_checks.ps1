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
    [switch]$InstallIfMissing,
    [int]$MinFreeSpaceGB = 8
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ranvierRoot = (Resolve-Path (Join-Path $scriptDir "..")).Path
Push-Location $ranvierRoot

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

function Get-WorkspaceDriveInfo {
    $workspaceRoot = (Get-Location).Path
    $driveName = ([System.IO.Path]::GetPathRoot($workspaceRoot)).TrimEnd('\').TrimEnd(':')
    if ([string]::IsNullOrWhiteSpace($driveName)) {
        return $null
    }
    return Get-PSDrive -Name $driveName -ErrorAction SilentlyContinue
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

    $driveInfo = Get-WorkspaceDriveInfo
    if ($null -ne $driveInfo) {
        $freeGb = [math]::Round($driveInfo.Free / 1GB, 2)
        Write-Log "Workspace drive free space: ${freeGb}GB"

        if ($freeGb -lt $MinFreeSpaceGB) {
            $failed = $true
            $note = "insufficient disk space (${freeGb}GB < ${MinFreeSpaceGB}GB)"
            Write-Log "[blocked_resource] $note"

            foreach ($pkg in $Packages) {
                $args = @("semver-checks", "check-release", "-p", $pkg) + $featureArgs
                $rows.Add([ordered]@{
                        package = $pkg
                        command = "cargo $($args -join ' ')"
                        feature_mode = $FeatureMode
                        exit_code = $null
                        result = "blocked_resource"
                        note = $note
                    })
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
            throw "Insufficient disk space for cargo-semver-checks workspace (need >= ${MinFreeSpaceGB}GB)"
        }
    }

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
        $outputText = $commandOutput -join "`n"
        $blockedByPolicy = $false
        $blockedByResource = $false
        $noLibraryTarget = $false
        if ($exitCode -ne 0) {
            $blockedByPolicy = $outputText -match "os error 4551"
            $blockedByResource = $outputText -match "os error 112|no space on device|디스크 공간이 부족"
            $noLibraryTarget = $outputText -match "no crates with library targets selected"
        }

        $result = if ($exitCode -eq 0) {
            "pass"
        } elseif ($noLibraryTarget) {
            "skip_non_library"
        } elseif ($blockedByPolicy) {
            "blocked_policy"
        } elseif ($blockedByResource) {
            "blocked_resource"
        } else {
            "fail"
        }

        $note = if ($blockedByPolicy) {
            "Windows application control policy (os error 4551)"
        } elseif ($noLibraryTarget) {
            "Crate has no library target; semver API surface check not applicable"
        } elseif ($blockedByResource) {
            "Insufficient disk space for semver-checks workspace (os error 112)"
        } else {
            $null
        }

        $rows.Add([ordered]@{
                package = $pkg
                command = "cargo $($args -join ' ')"
                feature_mode = $FeatureMode
                exit_code = $exitCode
                result = $result
                note = $note
            })

        if ($result -eq "fail" -or $result -eq "blocked_policy" -or $result -eq "blocked_resource") {
            $failed = $true
            Write-Log "[fail] $pkg (exit=$exitCode, result=$result)"
        } elseif ($result -eq "skip_non_library") {
            Write-Log "[skip] $pkg ($note)"
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
    Pop-Location
}
