param()

$ErrorActionPreference = "Stop"
$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
. (Join-Path $ScriptRoot "benchmark_common.ps1")

$RanvierRoot = Split-Path -Parent $ScriptRoot
$WorkspaceRoot = Split-Path -Parent $RanvierRoot
$EvidencePath = New-EvidencePath -WorkspaceRoot $WorkspaceRoot -Prefix "m384_dependency_drift_check"

function Write-Evidence {
    param([string]$Line)
    Add-Content -Path $EvidencePath -Value $Line
}

$targetFiles = @(
    (Join-Path $RanvierRoot "Cargo.toml"),
    (Join-Path $RanvierRoot "http\Cargo.toml"),
    (Join-Path $RanvierRoot "kit\Cargo.toml"),
    (Join-Path $RanvierRoot "examples\static-spa-demo\Cargo.toml"),
    (Join-Path $RanvierRoot "examples\experimental\fullstack-demo\Cargo.toml")
)

$forbidden = @("axum", "tower", "tower-http")
$violations = New-Object System.Collections.Generic.List[string]

Set-Content -Path $EvidencePath -Value @(
    "M384 dependency drift check",
    "Workspace: $WorkspaceRoot"
) -Encoding UTF8

foreach ($file in $targetFiles) {
    $content = Get-Content -Path $file
    foreach ($dep in $forbidden) {
        $pattern = "^\s*{0}\s*=" -f [regex]::Escape($dep)
        $matches = $content | Select-String -Pattern $pattern
        foreach ($match in $matches) {
            $relative = Resolve-Path -Relative $file
            $violations.Add("${relative}:$($match.LineNumber): $($match.Line.Trim())")
        }
    }
}

if ($violations.Count -gt 0) {
    Write-Evidence "violations:"
    foreach ($violation in $violations) {
        Write-Evidence " - $violation"
    }
    Write-Host "Evidence: $EvidencePath"
    throw "Forbidden dependencies found in the internal default path."
}

Write-Evidence "No forbidden dependency declarations found in the internal default path manifests."
Write-Host "Evidence: $EvidencePath"
