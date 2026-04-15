param()

$ErrorActionPreference = "Stop"
$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
. (Join-Path $ScriptRoot "benchmark_common.ps1")

$RanvierRoot = Split-Path -Parent $ScriptRoot
$WorkspaceRoot = Split-Path -Parent $RanvierRoot
$EvidencePath = New-EvidencePath -WorkspaceRoot $WorkspaceRoot -Prefix "m384_static_artifact_smoke"

function Write-Evidence {
    param([string]$Line)
    Add-Content -Path $EvidencePath -Value $Line
}

function Invoke-CargoTestFilter {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Filter
    )

    Write-Host "[m384-artifact-smoke] cargo test filter: $Filter"
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
    "M384 static artifact smoke",
    "Workspace: $WorkspaceRoot"
) -Encoding UTF8

Push-Location $RanvierRoot
try {
    Invoke-CargoTestFilter -Filter "react_vite_static_build_artifact_smoke"
    Write-Evidence "react/vite static build artifact smoke: PASS"

    Invoke-CargoTestFilter -Filter "sveltekit_adapter_static_artifact_smoke"
    Write-Evidence "sveltekit adapter-static artifact smoke: PASS"

    Invoke-CargoTestFilter -Filter "leptos_csr_prerender_artifact_smoke"
    Write-Evidence "leptos csr/prerender artifact smoke: PASS"
}
finally {
    Pop-Location
}

Write-Host "Evidence: $EvidencePath"
