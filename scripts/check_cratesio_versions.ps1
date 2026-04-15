param(
    [ValidateSet("m119", "m131", "all")]
    [string]$Profile = "all",
    [string[]]$Crates,
    [string]$TargetVersion,
    [int]$TimeoutSec = 20,
    [string]$EvidenceDir = "..\docs\05_dev_plans\evidence"
)

$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "release_common.ps1")

function Resolve-CrateSet {
    param(
        [string]$Key,
        [string]$WorkspaceRoot
    )

    return Resolve-ReleaseCrateSet -ProfileKey $Key -WorkspaceRoot $WorkspaceRoot
}

function Infer-TargetVersion {
    param(
        [string]$Requested,
        [string]$WorkspaceRoot
    )

    return Resolve-ReleaseTargetVersion -Requested $Requested -WorkspaceRoot $WorkspaceRoot
}

function Write-Log {
    param(
        [string]$Path,
        [string]$Message
    )
    $line = "[{0}] {1}" -f (Get-Date -Format "HH:mm:ss"), $Message
    Write-Host $line
    Add-Content -Path $Path -Value $line -Encoding utf8
}

$profileKey = $Profile.ToLowerInvariant()
$workspaceRoot = Get-ReleaseWorkspaceRoot -ScriptRoot $PSScriptRoot
$target = Infer-TargetVersion -Requested $TargetVersion -WorkspaceRoot $workspaceRoot
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$EvidenceDir = Resolve-ReleaseEvidenceDir -Requested $EvidenceDir -WorkspaceRoot $workspaceRoot

$crateList = @()
if ($null -ne $Crates -and $Crates.Count -gt 0) {
    $crateList = @($Crates | Sort-Object -Unique)
} else {
    $crateList = @((Resolve-CrateSet -Key $profileKey -WorkspaceRoot $workspaceRoot) | Sort-Object -Unique)
}

New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
$evidencePath = Join-Path $EvidenceDir "cratesio_version_snapshot_${profileKey}_${target}_${timestamp}.log"
$summaryPath = Join-Path $EvidenceDir "cratesio_version_snapshot_${profileKey}_${target}_${timestamp}.json"

Write-Log -Path $evidencePath -Message "crates.io version snapshot started (profile=$profileKey, target=$target)"
Write-Log -Path $evidencePath -Message "Crates: $($crateList -join ', ')"

$results = New-Object System.Collections.Generic.List[object]
$userAgent = "ranvier-release-check/1.0"

foreach ($crate in $crateList) {
    $url = "https://crates.io/api/v1/crates/$crate"
    try {
        $response = Invoke-RestMethod -Uri $url -Method Get -Headers @{ "User-Agent" = $userAgent } -TimeoutSec $TimeoutSec
        $versions = @($response.versions | Where-Object { -not $_.yanked } | ForEach-Object { [string]$_.num })
        $latest = $null
        if ($versions.Count -gt 0) {
            $latest = $versions[0]
        }

        $hasTarget = $false
        if (-not [string]::IsNullOrWhiteSpace($target)) {
            $hasTarget = ($versions -contains $target)
        }

        $result = [ordered]@{
            crate = $crate
            found = $true
            latest_non_yanked = $latest
            has_target = $hasTarget
            target_version = $target
            non_yanked_versions = @($versions)
            error = $null
        }
        $results.Add($result)

        Write-Log -Path $evidencePath -Message "${crate}: latest=$latest has_target=$hasTarget"
    } catch {
        $message = $_.Exception.Message
        $statusCode = $null
        if ($_.Exception.Response -and $_.Exception.Response.StatusCode) {
            $statusCode = [int]$_.Exception.Response.StatusCode
        }

        $result = [ordered]@{
            crate = $crate
            found = $false
            latest_non_yanked = $null
            has_target = $false
            target_version = $target
            non_yanked_versions = @()
            error = $message
            status_code = $statusCode
        }
        $results.Add($result)

        Write-Log -Path $evidencePath -Message "${crate}: request failed status=$statusCode message=$message"
    }
}

$found = @($results | Where-Object { $_.found })
$missingTarget = @($found | Where-Object { -not $_.has_target } | ForEach-Object { [string]$_.crate })
$targetPresent = @($found | Where-Object { $_.has_target } | ForEach-Object { [string]$_.crate })
$notFound = @($results | Where-Object { -not $_.found } | ForEach-Object { [string]$_.crate })

$summary = [ordered]@{
    timestamp = $timestamp
    profile = $profileKey
    target_version = $target
    total = $results.Count
    found = $found.Count
    not_found = $notFound.Count
    target_present_count = $targetPresent.Count
    target_missing_count = $missingTarget.Count
    target_present_crates = $targetPresent
    target_missing_crates = $missingTarget
    not_found_crates = $notFound
    results = $results
}

$summary | ConvertTo-Json -Depth 8 | Set-Content -Path $summaryPath -Encoding utf8

Write-Log -Path $evidencePath -Message "target_present_count=$($summary.target_present_count) target_missing_count=$($summary.target_missing_count)"
Write-Log -Path $evidencePath -Message "Summary JSON: $summaryPath"
Write-Host "Evidence: $evidencePath"
Write-Host "Summary:  $summaryPath"
