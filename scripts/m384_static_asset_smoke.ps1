param()

$ErrorActionPreference = "Stop"
$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
. (Join-Path $ScriptRoot "benchmark_common.ps1")

$RanvierRoot = Split-Path -Parent $ScriptRoot
$WorkspaceRoot = Split-Path -Parent $RanvierRoot
$EvidencePath = New-EvidencePath -WorkspaceRoot $WorkspaceRoot -Prefix "m384_static_asset_smoke"

function Write-Evidence {
    param([string]$Line)
    Add-Content -Path $EvidencePath -Value $Line
}

function Invoke-CargoTestFilter {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Filter
    )

    Write-Host "[m384-static-smoke] cargo test filter: $Filter"
    $tempOutput = [System.IO.Path]::GetTempFileName()
    try {
        $exitCode = Invoke-LoggedProcess `
            -FilePath "cargo" `
            -ArgumentList @("test", "-p", "ranvier-http", $Filter, "--", "--nocapture") `
            -OutputPath $tempOutput
        if (Test-Path $tempOutput) {
            Get-Content -Path $tempOutput -Encoding UTF8 | Tee-Object -FilePath $EvidencePath -Append | Out-Host
        }
    }
    finally {
        Remove-Item -Path $tempOutput -Force -ErrorAction SilentlyContinue
    }
    if ($exitCode -ne 0) {
        throw "cargo test failed for filter '$Filter'"
    }
}

Assert-CommandExists -Name "cargo" -InstallHint "Install Rust and ensure cargo is on PATH."

Set-Content -Path $EvidencePath -Value @(
    "M384 targeted static asset smoke",
    "Workspace: $WorkspaceRoot"
) -Encoding UTF8

Push-Location $RanvierRoot
try {
    foreach ($filter in @(
        "missing_static_asset_returns_404_instead_of_500",
        "traversal_attempt_returns_403",
        "head_static_response_returns_headers_without_body",
        "head_static_error_response_omits_body",
        "hashed_assets_and_spa_shell_use_different_cache_policies",
        "spa_shell_excludes_api_events_and_ws_paths"
    )) {
        Invoke-CargoTestFilter -Filter $filter
        Write-Evidence "${filter}: PASS"
    }
}
finally {
    Pop-Location
}

Write-Host "Evidence: $EvidencePath"
