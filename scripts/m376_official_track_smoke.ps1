param(
    [string]$WorkspaceRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
)

$ErrorActionPreference = "Stop"

function Wait-Http {
    param(
        [string]$Url,
        [int]$Attempts = 60,
        [int]$DelayMs = 500
    )

    for ($i = 0; $i -lt $Attempts; $i++) {
        try {
            Invoke-WebRequest -Uri $Url -UseBasicParsing -TimeoutSec 2 | Out-Null
            return
        } catch {
            Start-Sleep -Milliseconds $DelayMs
        }
    }

    throw "Timed out waiting for $Url"
}

function Start-ExampleBinary {
    param(
        [string]$BinaryName,
        [hashtable]$Environment = @{}
    )

    return Start-Job -ScriptBlock {
        param($Root, $ExeName, $EnvMap)
        foreach ($key in $EnvMap.Keys) {
            Set-Item -Path ("Env:" + $key) -Value $EnvMap[$key]
        }
        & (Join-Path $Root ("target/debug/" + $ExeName + ".exe"))
    } -ArgumentList $WorkspaceRoot, $BinaryName, $Environment
}

function Stop-CargoServer {
    param(
        [System.Management.Automation.Job]$Job
    )

    if ($null -ne $Job) {
        Stop-Job $Job -ErrorAction SilentlyContinue | Out-Null
        Remove-Job $Job -Force -ErrorAction SilentlyContinue | Out-Null
    }
}

Push-Location $WorkspaceRoot
try {
    Write-Host "[m376-smoke] Building official track packages..."
    cargo build -p hello-world -p reference-todo-api -p order-processing-demo -p admin-crud-demo

    Write-Host "[m376-smoke] Verifying order-processing-demo..."
    cargo run -p order-processing-demo | Out-Null

    Write-Host "[m376-smoke] Verifying hello-world HTTP path..."
    $hello = Start-ExampleBinary -BinaryName "hello-world"
    try {
        Wait-Http -Url "http://127.0.0.1:3000/"
        $helloResponse = Invoke-WebRequest -Uri "http://127.0.0.1:3000/" -UseBasicParsing
        if ($helloResponse.StatusCode -ne 200) {
            throw "hello-world returned status $($helloResponse.StatusCode)"
        }
    } finally {
        Stop-CargoServer -Job $hello
    }

    Write-Host "[m376-smoke] Verifying reference-todo-api login/list flow..."
    $todo = Start-ExampleBinary -BinaryName "reference-todo-api" -Environment @{ JWT_SECRET = "m376-smoke-secret" }
    try {
        Start-Sleep -Seconds 3
        $login = Invoke-WebRequest -Uri "http://127.0.0.1:3000/login" -Method Post -ContentType "application/json" -Body '{"username":"admin","password":"admin"}' -UseBasicParsing
        if ($login.StatusCode -ne 200) {
            throw "reference-todo-api login returned status $($login.StatusCode)"
        }
        $todos = Invoke-WebRequest -Uri "http://127.0.0.1:3000/todos" -UseBasicParsing
        if ($todos.StatusCode -ne 200) {
            throw "reference-todo-api list returned status $($todos.StatusCode)"
        }
    } finally {
        Stop-CargoServer -Job $todo
    }

    Write-Host "[m376-smoke] Verifying admin-crud-demo login/openapi flow..."
    $admin = Start-ExampleBinary -BinaryName "admin-crud-demo"
    try {
        Start-Sleep -Seconds 3
        $login = Invoke-WebRequest -Uri "http://127.0.0.1:3120/login" -Method Post -ContentType "application/json" -Body '{"username":"admin","password":"admin123"}' -UseBasicParsing
        if ($login.StatusCode -ne 200) {
            throw "admin-crud-demo login returned status $($login.StatusCode)"
        }
        $openapi = Invoke-WebRequest -Uri "http://127.0.0.1:3120/openapi.json" -UseBasicParsing
        if ($openapi.StatusCode -ne 200) {
            throw "admin-crud-demo openapi returned status $($openapi.StatusCode)"
        }
    } finally {
        Stop-CargoServer -Job $admin
    }

    Write-Host "[m376-smoke] OK"
}
finally {
    Pop-Location
}
