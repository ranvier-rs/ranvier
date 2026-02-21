param(
    [string]$EvidenceDir = "..\docs\05_dev_plans\evidence",
    [int]$StartupTimeoutSec = 120
)

$ErrorActionPreference = "Stop"

$workspaceRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$evidencePath = Join-Path $EvidenceDir "m131_inspector_quickview_smoke_$timestamp.log"
New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null

function Write-Log {
    param([string]$Message)
    $line = "[{0}] {1}" -f (Get-Date -Format "HH:mm:ss"), $Message
    $line | Tee-Object -FilePath $evidencePath -Append
}

function Invoke-Http {
    param(
        [string]$Uri,
        [int]$TimeoutSec = 2
    )
    try {
        $resp = Invoke-WebRequest -Uri $Uri -TimeoutSec $TimeoutSec -SkipHttpErrorCheck
        return @{
            Status = [int]$resp.StatusCode
            Headers = $resp.Headers
            Body = $resp.Content
        }
    } catch {
        return @{
            Status = 0
            Headers = @{}
            Body = $_.Exception.Message
        }
    }
}

function Wait-Endpoint {
    param(
        [string]$Uri,
        [int]$DeadlineSec = 30,
        [int[]]$ExpectedStatuses = @()
    )
    $deadline = (Get-Date).AddSeconds($DeadlineSec)
    while ((Get-Date) -lt $deadline) {
        $resp = Invoke-Http -Uri $Uri
        if ($resp.Status -ne 0 -and ($ExpectedStatuses.Count -eq 0 -or $ExpectedStatuses -contains $resp.Status)) {
            return $resp
        }
        Start-Sleep -Milliseconds 300
    }
    return $null
}

$demo = $null
$failures = New-Object System.Collections.Generic.List[string]

try {
    Write-Log "Starting studio-demo for inspector quick-view smoke"
    $stdoutPath = Join-Path $EvidenceDir "m131_studio-demo_${timestamp}_stdout.log"
    $stderrPath = Join-Path $EvidenceDir "m131_studio-demo_${timestamp}_stderr.log"
    $demo = Start-Process -FilePath "cargo" `
        -ArgumentList @("run", "-p", "studio-demo") `
        -WorkingDirectory $workspaceRoot `
        -RedirectStandardOutput $stdoutPath `
        -RedirectStandardError $stderrPath `
        -PassThru

    $quickView = Wait-Endpoint -Uri "http://127.0.0.1:9000/quick-view" -DeadlineSec $StartupTimeoutSec -ExpectedStatuses @(200)
    if ($null -eq $quickView) {
        throw "quick-view endpoint not ready"
    }
    if ($quickView.Body -notmatch "html") {
        throw "quick-view response does not look like html"
    }
    Write-Log "quick-view endpoint verified"

    $schematic = Wait-Endpoint -Uri "http://127.0.0.1:9000/schematic" -DeadlineSec 30 -ExpectedStatuses @(200)
    if ($null -eq $schematic) {
        throw "schematic endpoint not ready"
    }
    $schematicJson = $schematic.Body | ConvertFrom-Json
    if ($null -eq $schematicJson.nodes -or $schematicJson.nodes.Count -eq 0) {
        throw "schematic payload has no nodes"
    }
    Write-Log "schematic endpoint verified"

    $publicTrace = Wait-Endpoint -Uri "http://127.0.0.1:9000/trace/public" -DeadlineSec 30 -ExpectedStatuses @(200)
    if ($null -eq $publicTrace) {
        throw "trace/public endpoint not ready"
    }
    $publicJson = $publicTrace.Body | ConvertFrom-Json
    if ($null -eq $publicJson.circuits -or $publicJson.circuits.Count -eq 0) {
        throw "trace/public payload missing circuits data"
    }
    Write-Log "trace/public endpoint verified"

    $internalTrace = Wait-Endpoint -Uri "http://127.0.0.1:9000/trace/internal" -DeadlineSec 30 -ExpectedStatuses @(200)
    if ($null -eq $internalTrace) {
        throw "trace/internal endpoint not ready"
    }
    $internalJson = $internalTrace.Body | ConvertFrom-Json
    if ($null -eq $internalJson.nodes -or $internalJson.nodes.Count -eq 0) {
        throw "trace/internal payload missing nodes data"
    }
    Write-Log "trace/internal endpoint verified"
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
    Write-Log "Inspector quick-view smoke failed:"
    foreach ($failure in $failures) {
        Write-Log " - $failure"
    }
    Write-Host "Evidence: $evidencePath"
    exit 1
}

Write-Log "Inspector quick-view smoke succeeded"
Write-Host "Evidence: $evidencePath"
