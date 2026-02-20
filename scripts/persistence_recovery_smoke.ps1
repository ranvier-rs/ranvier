param(
    [string]$EvidenceDir = ""
)

$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ranvierRoot = (Resolve-Path (Join-Path $scriptDir "..")).Path
$workspaceRoot = (Resolve-Path (Join-Path $ranvierRoot "..")).Path

if ([string]::IsNullOrWhiteSpace($EvidenceDir)) {
    $EvidenceDir = Join-Path $workspaceRoot "docs/05_dev_plans/evidence"
}

New-Item -ItemType Directory -Path $EvidenceDir -Force | Out-Null

$stamp = Get-Date -Format "yyyyMMdd_HHmmss"
$appLog = Join-Path $EvidenceDir ("persistence_recovery_demo_smoke_" + $stamp + ".log")

Push-Location $ranvierRoot
try {
    cargo run -p persistence-recovery-demo *>&1 | Tee-Object -FilePath $appLog | Out-Null
} finally {
    Pop-Location
}

$requiredPatterns = @(
    'first run outcome: Fault("payment_declined")',
    'resume cursor: trace_id=order-1001 next_step=2',
    'second run outcome: Next("tracking-1001")',
    '[compensate] trace=order-2001 transient failure, retry pending',
    'third run (compensation) outcome: Fault("payment_declined")',
    'completion: Some(Compensated)'
)

foreach ($pattern in $requiredPatterns) {
    if (-not (Select-String -Path $appLog -Pattern $pattern -SimpleMatch)) {
        throw "Persistence recovery smoke failed. Missing expected pattern: $pattern"
    }
}

Write-Host "Persistence recovery smoke passed."
Write-Host "APP_LOG=$appLog"
