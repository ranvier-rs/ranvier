param(
    [string]$EvidenceDir = "..\docs\05_dev_plans\evidence",
    [int]$StartupTimeoutSec = 90
)

$ErrorActionPreference = "Stop"

$workspaceRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$evidencePath = Join-Path $EvidenceDir "m131_fullstack_embedded_smoke_$timestamp.log"
$stdoutPath = Join-Path $EvidenceDir "m131_fullstack_embedded_app_$timestamp.stdout.log"
$stderrPath = Join-Path $EvidenceDir "m131_fullstack_embedded_app_$timestamp.stderr.log"
New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null

function Write-Log {
    param([string]$Message)
    $line = "[{0}] {1}" -f (Get-Date -Format "HH:mm:ss"), $Message
    $line | Tee-Object -FilePath $evidencePath -Append
}

function Invoke-Http {
    param(
        [string]$Uri,
        [string]$Method = "GET",
        [hashtable]$Headers = @{},
        [string]$Body = "",
        [int]$TimeoutSec = 3
    )
    try {
        $resp = Invoke-WebRequest -Uri $Uri -Method $Method -Headers $Headers -Body $Body -TimeoutSec $TimeoutSec -SkipHttpErrorCheck
        return @{
            Status = [int]$resp.StatusCode
            Body = $resp.Content
        }
    } catch {
        return @{
            Status = 0
            Body = $_.Exception.Message
        }
    }
}

function Wait-Endpoint {
    param(
        [string]$Uri,
        [int]$DeadlineSec = 30
    )
    $deadline = (Get-Date).AddSeconds($DeadlineSec)
    while ((Get-Date) -lt $deadline) {
        $resp = Invoke-Http -Uri $Uri
        if ($resp.Status -eq 200) {
            return $true
        }
        Start-Sleep -Milliseconds 250
    }
    return $false
}

$demo = $null
$failures = New-Object System.Collections.Generic.List[string]

try {
    Write-Log "Starting fullstack-demo"
    $demo = Start-Process -FilePath "cargo" `
        -ArgumentList @("run", "-p", "fullstack-demo") `
        -WorkingDirectory $workspaceRoot `
        -RedirectStandardOutput $stdoutPath `
        -RedirectStandardError $stderrPath `
        -PassThru

    if (-not (Wait-Endpoint -Uri "http://127.0.0.1:3030/" -DeadlineSec $StartupTimeoutSec)) {
        throw "fullstack-demo endpoint not ready"
    }

    $root = Invoke-Http -Uri "http://127.0.0.1:3030/"
    if ($root.Status -ne 200 -or $root.Body -notmatch "Embedded Fullstack Demo") {
        throw "root page verification failed (status=$($root.Status))"
    }
    Write-Log "root page verified"

    $asset = Invoke-Http -Uri "http://127.0.0.1:3030/assets/app.js"
    if ($asset.Status -ne 200) {
        throw "asset verification failed (status=$($asset.Status))"
    }
    Write-Log "asset serving verified"

    $spa = Invoke-Http -Uri "http://127.0.0.1:3030/dashboard/settings"
    if ($spa.Status -ne 200 -or $spa.Body -notmatch "Embedded Fullstack Demo") {
        throw "spa fallback verification failed (status=$($spa.Status))"
    }
    Write-Log "spa fallback verified"

    $api = Invoke-Http -Uri "http://127.0.0.1:3030/api/order" `
        -Method "POST" `
        -Headers @{ "content-type" = "application/json" } `
        -Body '{"item":"smoke","qty":1}'
    if ($api.Status -ne 200) {
        throw "api verification failed (status=$($api.Status))"
    }
    $apiBody = $api.Body | ConvertFrom-Json
    if ($apiBody.status -ne "accepted") {
        throw "api payload status mismatch: $($apiBody.status)"
    }
    Write-Log "api route verified"
} catch {
    $failures.Add($_.Exception.Message)
} finally {
    if ($null -ne $demo) {
        try {
            if (-not $demo.HasExited) {
                Stop-Process -Id $demo.Id -Force
            }
        } catch {
            # ignore
        }
    }
}

if ($failures.Count -gt 0) {
    Write-Log "fullstack embedded smoke failed:"
    foreach ($failure in $failures) {
        Write-Log " - $failure"
    }
    Write-Host "Evidence: $evidencePath"
    exit 1
}

Write-Log "fullstack embedded smoke succeeded"
Write-Host "Evidence: $evidencePath"
