param(
    [int]$StartupTimeoutSeconds = 30
)

$ErrorActionPreference = "Stop"
$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
. (Join-Path $ScriptRoot "benchmark_common.ps1")

$RanvierRoot = Split-Path -Parent $ScriptRoot
$WorkspaceRoot = Split-Path -Parent $RanvierRoot
$EvidencePath = New-EvidencePath -WorkspaceRoot $WorkspaceRoot -Prefix "m386_openapi_parity"

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

$openapiProcess = $null
$adminProcess = $null
$governanceProcess = $null

Push-Location $RanvierRoot
try {
    Set-Content -Path $EvidencePath -Value @(
        "M386 OpenAPI parity smoke",
        "Workspace: $WorkspaceRoot",
        "Rerun: powershell -NoProfile -ExecutionPolicy Bypass -File ranvier/scripts/m386_openapi_parity_smoke.ps1"
    ) -Encoding UTF8

    Write-Host "[m386-smoke] Building target examples..."
    cargo build -p openapi-demo -p admin-crud-demo -p request-governance-demo
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed for M386 smoke targets."
    }

    $openapiExe = Join-Path $RanvierRoot "target\debug\openapi-demo.exe"
    $adminExe = Join-Path $RanvierRoot "target\debug\admin-crud-demo.exe"
    $governanceExe = Join-Path $RanvierRoot "target\debug\request-governance-demo.exe"

    Write-Host "[m386-smoke] Verifying openapi-demo generator/spec parity..."
    $openapiProcess = Start-ExampleProcess -ExePath $openapiExe
    Wait-TcpPort -Address "127.0.0.1" -Port 3111 -TimeoutSeconds $StartupTimeoutSeconds

    $openapiResponse = Invoke-WebRequest -Uri "http://127.0.0.1:3111/openapi.json" -UseBasicParsing
    if ($openapiResponse.StatusCode -ne 200) {
        throw "openapi-demo /openapi.json returned status $($openapiResponse.StatusCode)"
    }

    $openapi = $openapiResponse.Content | ConvertFrom-Json
    foreach ($path in @("/users/{id}", "/users/me", "/users", "/healthz", "/readyz", "/livez")) {
        if (-not $openapi.paths.PSObject.Properties.Name.Contains($path)) {
            throw "openapi-demo OpenAPI document does not contain $path"
        }
    }

    $meSecurity = $openapi.paths."/users/me".get.security
    if ($null -eq $meSecurity -or $meSecurity.Count -lt 1) {
        throw "openapi-demo /users/me does not contain a security requirement"
    }

    $meRequirement = $meSecurity[0].PSObject.Properties.Name
    if (-not $meRequirement.Contains("bearerAuth")) {
        throw "openapi-demo /users/me security requirement does not contain bearerAuth"
    }

    $postUsers = $openapi.paths."/users".post
    if ($null -eq $postUsers.requestBody.content."application/json") {
        throw "openapi-demo POST /users is missing application/json requestBody"
    }
    foreach ($code in @("400", "404", "500")) {
        if ($null -eq $postUsers.responses.$code) {
            throw "openapi-demo POST /users missing ProblemDetail response $code"
        }
    }

    $runtimeUser = Invoke-WebRequest -Uri "http://127.0.0.1:3111/users/42" -UseBasicParsing
    if ($runtimeUser.StatusCode -ne 200) {
        throw "openapi-demo GET /users/42 returned status $($runtimeUser.StatusCode)"
    }
    $runtimeUserJson = $runtimeUser.Content | ConvertFrom-Json
    if ($runtimeUserJson.id -ne "42") {
        throw "openapi-demo GET /users/42 did not return the expected id"
    }

    $runtimeMe = Invoke-WebRequest -Uri "http://127.0.0.1:3111/users/me" -Headers @{ Authorization = "Bearer demo-token" } -UseBasicParsing
    if ($runtimeMe.StatusCode -ne 200) {
        throw "openapi-demo GET /users/me returned status $($runtimeMe.StatusCode)"
    }

    $runtimeCreate = Invoke-WebRequest -Method Post -Uri "http://127.0.0.1:3111/users" -ContentType "application/json" -Body '{"email":"m386@example.com"}' -UseBasicParsing
    if ($runtimeCreate.StatusCode -ne 200) {
        throw "openapi-demo POST /users returned status $($runtimeCreate.StatusCode)"
    }

    foreach ($opsPath in @("http://127.0.0.1:3111/healthz", "http://127.0.0.1:3111/readyz", "http://127.0.0.1:3111/livez")) {
        $opsResponse = Invoke-WebRequest -Uri $opsPath -UseBasicParsing
        if ($opsResponse.StatusCode -ne 200) {
            throw "openapi-demo ops endpoint $opsPath returned status $($opsResponse.StatusCode)"
        }
    }

    $docsResponse = Invoke-WebRequest -Uri "http://127.0.0.1:3111/docs" -UseBasicParsing
    if ($docsResponse.StatusCode -ne 200) {
        throw "openapi-demo /docs returned status $($docsResponse.StatusCode)"
    }
    if ($docsResponse.Content -notmatch "/openapi\.json") {
        throw "openapi-demo /docs does not reference /openapi.json"
    }

    Write-Evidence "openapi-demo: generator/spec parity OK"
    Stop-ExampleProcess -Process $openapiProcess
    $openapiProcess = $null

    Write-Host "[m386-smoke] Verifying admin-crud-demo authenticated docs sanity..."
    $adminProcess = Start-ExampleProcess -ExePath $adminExe
    Wait-TcpPort -Address "127.0.0.1" -Port 3120 -TimeoutSeconds $StartupTimeoutSeconds

    $adminOpenapi = Invoke-WebRequest -Uri "http://127.0.0.1:3120/openapi.json" -UseBasicParsing
    if ($adminOpenapi.StatusCode -ne 200) {
        throw "admin-crud-demo /openapi.json returned status $($adminOpenapi.StatusCode)"
    }
    $adminDoc = $adminOpenapi.Content | ConvertFrom-Json
    if (-not $adminDoc.paths.PSObject.Properties.Name.Contains("/users")) {
        throw "admin-crud-demo OpenAPI document does not contain /users path"
    }

    $adminDocs = Invoke-WebRequest -Uri "http://127.0.0.1:3120/docs" -UseBasicParsing
    if ($adminDocs.StatusCode -ne 200) {
        throw "admin-crud-demo /docs returned status $($adminDocs.StatusCode)"
    }
    if ($adminDocs.Content -notmatch "/openapi\.json") {
        throw "admin-crud-demo /docs does not reference /openapi.json"
    }

    Write-Evidence "admin-crud-demo: authenticated OpenAPI/docs sanity OK"
    Stop-ExampleProcess -Process $adminProcess
    $adminProcess = $null

    Write-Host "[m386-smoke] Verifying request-governance-demo explicit ProblemDetail runtime path..."
    $governanceProcess = Start-ExampleProcess -ExePath $governanceExe
    Wait-TcpPort -Address "127.0.0.1" -Port 3140 -TimeoutSeconds $StartupTimeoutSeconds

    $aliceLogin = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:3140/login" -ContentType "application/json" -Body '{"username":"alice","password":"alice123"}'
    if (-not $aliceLogin.token) {
        throw "request-governance-demo login did not return a token"
    }

    $createResponse = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:3140/requests" -ContentType "application/json" -Headers @{ authorization = "Bearer $($aliceLogin.token)" } -Body '{"title":"m386 smoke request"}'
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

    $governanceOpenapi = Invoke-HttpRequestRaw -Method "GET" -Uri "http://127.0.0.1:3140/openapi.json"
    if ($governanceOpenapi.StatusCode -ne 404) {
        throw "request-governance-demo /openapi.json returned status $($governanceOpenapi.StatusCode), expected 404"
    }

    Write-Evidence "request-governance-demo: explicit ProblemDetail runtime path OK; OpenAPI intentionally absent"
    Write-Host "Evidence: $EvidencePath"
}
finally {
    Pop-Location
    Stop-ExampleProcess -Process $openapiProcess
    Stop-ExampleProcess -Process $adminProcess
    Stop-ExampleProcess -Process $governanceProcess
}
