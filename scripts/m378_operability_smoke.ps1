param(
    [int]$StartupTimeoutSeconds = 30
)

$ErrorActionPreference = "Stop"
$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
. (Join-Path $ScriptRoot "benchmark_common.ps1")

$RanvierRoot = Split-Path -Parent $ScriptRoot
$WorkspaceRoot = Split-Path -Parent $RanvierRoot
$EvidencePath = New-EvidencePath -WorkspaceRoot $WorkspaceRoot -Prefix "m378_operability_smoke"

function Write-Evidence {
    param([string]$Line)
    Add-Content -Path $EvidencePath -Value $Line
}

function Start-ExampleProcess {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ExePath
    )

    if (-not (Test-Path $ExePath)) {
        throw "Expected executable not found: $ExePath"
    }

    return Start-Process -FilePath $ExePath -PassThru -NoNewWindow
}

function Stop-ExampleProcess {
    param($Process)
    if ($Process -and -not $Process.HasExited) {
        Stop-Process -Id $Process.Id -Force
    }
}

function Invoke-HttpRequestRaw {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Method,
        [Parameter(Mandatory = $true)]
        [string]$Uri,
        [hashtable]$Headers = @{},
        [string]$ContentType = "",
        [string]$Body = ""
    )

    $request = [System.Net.HttpWebRequest]::Create($Uri)
    $request.Method = $Method
    if ($ContentType) {
        $request.ContentType = $ContentType
    }
    foreach ($key in $Headers.Keys) {
        if ($key -ieq "Content-Type") {
            $request.ContentType = $Headers[$key]
        } else {
            $request.Headers[$key] = $Headers[$key]
        }
    }

    if ($Body) {
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($Body)
        $request.ContentLength = $bytes.Length
        $stream = $request.GetRequestStream()
        try {
            $stream.Write($bytes, 0, $bytes.Length)
        }
        finally {
            $stream.Dispose()
        }
    }

    try {
        $response = $request.GetResponse()
    }
    catch [System.Net.WebException] {
        $response = $_.Exception.Response
        if ($null -eq $response) {
            throw
        }
    }

    try {
        $reader = New-Object System.IO.StreamReader($response.GetResponseStream())
        try {
            $content = $reader.ReadToEnd()
        }
        finally {
            $reader.Dispose()
        }

        $headers = @{}
        foreach ($key in $response.Headers.AllKeys) {
            $headers[$key] = $response.Headers[$key]
        }

        return [pscustomobject]@{
            StatusCode = [int]$response.StatusCode
            Content = $content
            Headers = $headers
        }
    }
    finally {
        $response.Close()
    }
}

Assert-CommandExists -Name "cargo" -InstallHint "Install Rust and ensure cargo is on PATH."

$adminProcess = $null
$governanceProcess = $null

Push-Location $RanvierRoot
try {
    Set-Content -Path $EvidencePath -Value @(
        "M378 operability smoke",
        "Workspace: $WorkspaceRoot"
    ) -Encoding UTF8

    Write-Host "[m378-smoke] Building target examples..."
    cargo build -p admin-crud-demo -p request-governance-demo
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed for M378 smoke targets."
    }

    $adminExe = Join-Path $RanvierRoot "target\debug\admin-crud-demo.exe"
    $governanceExe = Join-Path $RanvierRoot "target\debug\request-governance-demo.exe"

    Write-Host "[m378-smoke] Verifying admin-crud-demo OpenAPI/docs parity..."
    $adminProcess = Start-ExampleProcess -ExePath $adminExe
    Wait-TcpPort -Address "127.0.0.1" -Port 3120 -TimeoutSeconds $StartupTimeoutSeconds

    $openapiResponse = Invoke-WebRequest -Uri "http://127.0.0.1:3120/openapi.json" -UseBasicParsing
    if ($openapiResponse.StatusCode -ne 200) {
        throw "admin-crud-demo /openapi.json returned status $($openapiResponse.StatusCode)"
    }
    $openapi = $openapiResponse.Content | ConvertFrom-Json
    if (-not $openapi.paths.PSObject.Properties.Name.Contains("/users")) {
        throw "admin-crud-demo OpenAPI document does not contain /users path"
    }

    $docsResponse = Invoke-WebRequest -Uri "http://127.0.0.1:3120/docs" -UseBasicParsing
    if ($docsResponse.StatusCode -ne 200) {
        throw "admin-crud-demo /docs returned status $($docsResponse.StatusCode)"
    }
    if ($docsResponse.Content -notmatch "/openapi\.json") {
        throw "admin-crud-demo /docs does not reference /openapi.json"
    }

    Write-Evidence "admin-crud-demo: /openapi.json and /docs OK"
    Stop-ExampleProcess -Process $adminProcess
    $adminProcess = $null

    Write-Host "[m378-smoke] Verifying request-governance-demo RFC 7807-style error path..."
    $governanceProcess = Start-ExampleProcess -ExePath $governanceExe
    Wait-TcpPort -Address "127.0.0.1" -Port 3140 -TimeoutSeconds $StartupTimeoutSeconds

    $aliceLogin = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:3140/login" -ContentType "application/json" -Body '{"username":"alice","password":"alice123"}'
    if (-not $aliceLogin.token) {
        throw "request-governance-demo login did not return a token"
    }

    $createResponse = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:3140/requests" -ContentType "application/json" -Headers @{ authorization = "Bearer $($aliceLogin.token)" } -Body '{"title":"m378 smoke request"}'
    if (-not $createResponse.id) {
        throw "request-governance-demo create request did not return an id"
    }

    $approveResponse = Invoke-HttpRequestRaw -Method "POST" -Uri "http://127.0.0.1:3140/requests/$($createResponse.id)/approve" -Headers @{ authorization = "Bearer $($aliceLogin.token)" }
    if ($approveResponse.StatusCode -ne 403) {
        throw "request-governance-demo approve-by-alice returned status $($approveResponse.StatusCode), expected 403"
    }
    if ($approveResponse.Headers["Content-Type"] -notmatch "application/problem\+json") {
        throw "request-governance-demo 403 response is not application/problem+json"
    }

    if ($approveResponse.Content -notmatch '"title"\s*:\s*"Forbidden"') {
        throw "request-governance-demo 403 body did not contain ProblemDetail title 'Forbidden'"
    }
    if ($approveResponse.Content -notmatch '"status"\s*:\s*403') {
        throw "request-governance-demo 403 body did not contain ProblemDetail status 403"
    }

    Write-Evidence "request-governance-demo: 403 problem detail OK"
    Write-Host "[m378-smoke] OK"
}
finally {
    Pop-Location
    Stop-ExampleProcess -Process $adminProcess
    Stop-ExampleProcess -Process $governanceProcess
}
