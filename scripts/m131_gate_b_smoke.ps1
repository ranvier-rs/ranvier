param(
    [string]$EvidenceDir = "..\docs\05_dev_plans\evidence",
    [int]$StartupTimeoutSec = 120
)

$ErrorActionPreference = "Stop"

$workspaceRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$evidencePath = Join-Path $EvidenceDir "m131_gate_b_smoke_$timestamp.log"
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
        [int]$TimeoutSec = 2
    )
    try {
        $resp = Invoke-WebRequest -Uri $Uri -Method $Method -Headers $Headers -TimeoutSec $TimeoutSec -SkipHttpErrorCheck
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
        Start-Sleep -Milliseconds 250
    }
    return $null
}

function Start-Demo {
    param(
        [string]$Package
    )
    $stdoutPath = Join-Path $EvidenceDir "m131_${Package}_${timestamp}_stdout.log"
    $stderrPath = Join-Path $EvidenceDir "m131_${Package}_${timestamp}_stderr.log"
    $proc = Start-Process -FilePath "cargo" `
        -ArgumentList @("run", "-p", $Package) `
        -WorkingDirectory $workspaceRoot `
        -RedirectStandardOutput $stdoutPath `
        -RedirectStandardError $stderrPath `
        -PassThru
    return @{
        Process = $proc
        StdoutPath = $stdoutPath
        StderrPath = $stderrPath
    }
}

function Stop-Demo {
    param($Demo)
    if ($null -ne $Demo -and $null -ne $Demo.Process) {
        try {
            if (-not $Demo.Process.HasExited) {
                Stop-Process -Id $Demo.Process.Id -Force
            }
        } catch {
            # ignore
        }
    }
}

$failures = New-Object System.Collections.Generic.List[string]

# 1) auth-jwt-role-demo
$authDemo = $null
try {
    Write-Log "Starting auth-jwt-role-demo"
    $authDemo = Start-Demo -Package "auth-jwt-role-demo"
    $authReady = Wait-Endpoint -Uri "http://127.0.0.1:3107/admin" -DeadlineSec $StartupTimeoutSec -ExpectedStatuses @(200, 401)
    if ($null -eq $authReady) {
        throw "auth demo endpoint not ready"
    }

    $token = $null
    $deadline = (Get-Date).AddSeconds(15)
    while ((Get-Date) -lt $deadline -and -not $token) {
        if (Test-Path $authDemo.StdoutPath) {
            $content = Get-Content $authDemo.StdoutPath -Raw
            $match = [regex]::Match($content, "Bearer\s+([^\r\n]+)")
            if ($match.Success) {
                $token = $match.Groups[1].Value
                break
            }
        }
        Start-Sleep -Milliseconds 300
    }
    if (-not $token) {
        throw "failed to parse demo token from auth log"
    }

    $unauth = Invoke-Http -Uri "http://127.0.0.1:3107/admin"
    if ($unauth.Status -ne 401) {
        throw "expected 401 for unauthenticated request, got $($unauth.Status)"
    }

    $auth = Invoke-Http -Uri "http://127.0.0.1:3107/admin" -Headers @{ Authorization = "Bearer $token" }
    if ($auth.Status -ne 200) {
        throw "expected 200 for authenticated request, got $($auth.Status)"
    }
    if ($auth.Body -notmatch "admin access granted") {
        throw "authenticated response body missing admin marker"
    }
    Write-Log "auth-jwt-role-demo verified"
} catch {
    $failures.Add("auth-jwt-role-demo: $($_.Exception.Message)")
} finally {
    Stop-Demo $authDemo
}

# 2) guard-demo
$guardDemo = $null
try {
    Write-Log "Starting guard-demo"
    $guardDemo = Start-Demo -Package "guard-demo"
    $guardReady = Wait-Endpoint -Uri "http://127.0.0.1:3110/public" -DeadlineSec $StartupTimeoutSec -ExpectedStatuses @(200)
    if ($null -eq $guardReady) {
        throw "guard demo endpoint not ready"
    }

    $corsCheck = Invoke-Http -Uri "http://127.0.0.1:3110/public" -Headers @{
        Origin = "http://localhost:5173"
    }
    if ($corsCheck.Status -ne 200) {
        throw "public route failed with status $($corsCheck.Status)"
    }
    $allowOriginRaw = $corsCheck.Headers["access-control-allow-origin"]
    $allowOrigin = if ($allowOriginRaw -is [System.Array]) { $allowOriginRaw -join "," } else { [string]$allowOriginRaw }
    if ($allowOrigin -ne "http://localhost:5173") {
        throw "unexpected access-control-allow-origin header: '$allowOrigin'"
    }

    $seen429 = $false
    for ($i = 0; $i -lt 8; $i++) {
        $burst = Invoke-Http -Uri "http://127.0.0.1:3110/burst" -Headers @{ "x-client-id" = "smoke-client" }
        if ($burst.Status -eq 429) {
            $seen429 = $true
            break
        }
    }
    if (-not $seen429) {
        throw "expected at least one 429 response on /burst"
    }
    Write-Log "guard-demo verified"
} catch {
    $failures.Add("guard-demo: $($_.Exception.Message)")
} finally {
    Stop-Demo $guardDemo
}

# 3) openapi-demo
$openapiDemo = $null
try {
    Write-Log "Starting openapi-demo"
    $openapiDemo = Start-Demo -Package "openapi-demo"
    $openapiReady = Wait-Endpoint -Uri "http://127.0.0.1:3111/openapi.json" -DeadlineSec $StartupTimeoutSec -ExpectedStatuses @(200)
    if ($null -eq $openapiReady) {
        throw "openapi endpoint not ready"
    }

    $openapi = Invoke-Http -Uri "http://127.0.0.1:3111/openapi.json"
    if ($openapi.Status -ne 200) {
        throw "openapi.json failed with status $($openapi.Status)"
    }
    $json = $openapi.Body | ConvertFrom-Json
    if ($null -eq $json.paths."/users/{id}") {
        throw "openapi.json missing /users/{id} path"
    }

    $docs = Invoke-Http -Uri "http://127.0.0.1:3111/docs"
    if ($docs.Status -ne 200) {
        throw "/docs failed with status $($docs.Status)"
    }
    if ($docs.Body -notmatch "swagger-ui") {
        throw "/docs body does not include swagger-ui marker"
    }
    Write-Log "openapi-demo verified"
} catch {
    $failures.Add("openapi-demo: $($_.Exception.Message)")
} finally {
    Stop-Demo $openapiDemo
}

# 4) static-spa-demo
$staticDemo = $null
try {
    Write-Log "Starting static-spa-demo"
    $staticDemo = Start-Demo -Package "static-spa-demo"
    $staticReady = Wait-Endpoint -Uri "http://127.0.0.1:3112/static/app.js" -DeadlineSec $StartupTimeoutSec -ExpectedStatuses @(200)
    if ($null -eq $staticReady) {
        throw "static endpoint not ready"
    }

    $asset = Invoke-Http -Uri "http://127.0.0.1:3112/static/app.js"
    if ($asset.Status -ne 200) {
        throw "static asset failed with status $($asset.Status)"
    }

    $spa = Invoke-Http -Uri "http://127.0.0.1:3112/dashboard/settings"
    if ($spa.Status -ne 200) {
        throw "spa fallback failed with status $($spa.Status)"
    }
    if ($spa.Body -notmatch "<html") {
        throw "spa fallback body missing html content"
    }
    Write-Log "static-spa-demo verified"
} catch {
    $failures.Add("static-spa-demo: $($_.Exception.Message)")
} finally {
    Stop-Demo $staticDemo
}

if ($failures.Count -gt 0) {
    Write-Log "Gate B smoke failed:"
    foreach ($failure in $failures) {
        Write-Log " - $failure"
    }
    Write-Host "Evidence: $evidencePath"
    exit 1
}

Write-Log "Gate B smoke succeeded"
Write-Host "Evidence: $evidencePath"
