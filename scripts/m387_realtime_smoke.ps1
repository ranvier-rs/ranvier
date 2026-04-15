param(
    [int]$StartupTimeoutSeconds = 30
)

$ErrorActionPreference = "Stop"
$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
. (Join-Path $ScriptRoot "benchmark_common.ps1")

$RanvierRoot = Split-Path -Parent $ScriptRoot
$WorkspaceRoot = Split-Path -Parent $RanvierRoot
$EvidencePath = New-EvidencePath -WorkspaceRoot $WorkspaceRoot -Prefix "m387_realtime_operability"

function Write-Evidence {
    param([string]$Line)
    Add-Content -Path $EvidencePath -Value $Line
}

function Start-ExampleProcess {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ExePath,
        [hashtable]$Environment = @{}
    )

    if (-not (Test-Path $ExePath)) {
        throw "Expected executable not found: $ExePath"
    }

    $previous = @{}
    foreach ($key in $Environment.Keys) {
        $previous[$key] = [System.Environment]::GetEnvironmentVariable($key, "Process")
        [System.Environment]::SetEnvironmentVariable($key, [string]$Environment[$key], "Process")
    }

    try {
        return Start-Process -FilePath $ExePath -PassThru -WindowStyle Hidden
    }
    finally {
        foreach ($key in $Environment.Keys) {
            [System.Environment]::SetEnvironmentVariable($key, $previous[$key], "Process")
        }
    }
}

function Stop-ExampleProcess {
    param($Process)
    if ($Process -and -not $Process.HasExited) {
        Stop-Process -Id $Process.Id -Force
        Wait-Process -Id $Process.Id -Timeout 5 -ErrorAction SilentlyContinue
    }
}

function Stop-StaleProcessByName {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Name
    )

    Get-Process -Name $Name -ErrorAction SilentlyContinue | ForEach-Object {
        Stop-Process -Id $_.Id -Force
    }
}

function Receive-WebSocketText {
    param(
        [Parameter(Mandatory = $true)]
        [System.Net.WebSockets.ClientWebSocket]$Socket,
        [int]$TimeoutSeconds = 5
    )

    $buffer = New-Object byte[] 4096
    $segment = [ArraySegment[byte]]::new($buffer)
    $stream = New-Object System.IO.MemoryStream
    $cts = [System.Threading.CancellationTokenSource]::new([TimeSpan]::FromSeconds($TimeoutSeconds))
    try {
        do {
            $result = $Socket.ReceiveAsync($segment, $cts.Token).GetAwaiter().GetResult()
            if ($result.MessageType -eq [System.Net.WebSockets.WebSocketMessageType]::Close) {
                return $null
            }
            $stream.Write($buffer, 0, $result.Count)
        } while (-not $result.EndOfMessage)
        return [System.Text.Encoding]::UTF8.GetString($stream.ToArray())
    }
    finally {
        $stream.Dispose()
        $cts.Dispose()
    }
}

function Send-WebSocketText {
    param(
        [Parameter(Mandatory = $true)]
        [System.Net.WebSockets.ClientWebSocket]$Socket,
        [Parameter(Mandatory = $true)]
        [string]$Text,
        [int]$TimeoutSeconds = 5
    )

    $bytes = [System.Text.Encoding]::UTF8.GetBytes($Text)
    $segment = [ArraySegment[byte]]::new($bytes)
    $cts = [System.Threading.CancellationTokenSource]::new([TimeSpan]::FromSeconds($TimeoutSeconds))
    try {
        $null = $Socket.SendAsync(
            $segment,
            [System.Net.WebSockets.WebSocketMessageType]::Text,
            $true,
            $cts.Token
        ).GetAwaiter().GetResult()
    }
    finally {
        $cts.Dispose()
    }
}

function Connect-WebSocket {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Uri,
        [int]$TimeoutSeconds = 5
    )

    $socket = [System.Net.WebSockets.ClientWebSocket]::new()
    $cts = [System.Threading.CancellationTokenSource]::new([TimeSpan]::FromSeconds($TimeoutSeconds))
    try {
        $null = $socket.ConnectAsync([Uri]$Uri, $cts.Token).GetAwaiter().GetResult()
        return $socket
    }
    finally {
        $cts.Dispose()
    }
}

Assert-CommandExists -Name "cargo" -InstallHint "Install Rust and ensure cargo is on PATH."

$chatProcess = $null
$streamProcess = $null
$unauthSocket = $null
$authSocket = $null

Push-Location $RanvierRoot
try {
    Set-Content -Path $EvidencePath -Value @(
        "M387 realtime operability smoke",
        "Workspace: $WorkspaceRoot",
        "Canonical WebSocket reference: reference-chat-server",
        "Canonical SSE reference: streaming-demo",
        "Rerun: powershell -NoProfile -ExecutionPolicy Bypass -File ranvier/scripts/m387_realtime_smoke.ps1"
    ) -Encoding UTF8

    Stop-StaleProcessByName -Name "reference-chat-server"
    Stop-StaleProcessByName -Name "streaming-demo"

    Write-Host "[m387-smoke] Building target examples..."
    cargo build -p reference-chat-server -p streaming-demo
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed for M387 smoke targets."
    }

    $chatExe = Join-Path $RanvierRoot "target\debug\reference-chat-server.exe"
    $streamExe = Join-Path $RanvierRoot "target\debug\streaming-demo.exe"

    Write-Host "[m387-smoke] Verifying reference-chat-server websocket/auth surface..."
    $chatPort = 3310
    $chatProcess = Start-ExampleProcess -ExePath $chatExe -Environment @{
        "RANVIER_SERVER_HOST" = "127.0.0.1"
        "RANVIER_SERVER_PORT" = "$chatPort"
        "RANVIER_INSPECTOR_PORT" = "3311"
    }
    Wait-TcpPort -Address "127.0.0.1" -Port $chatPort -TimeoutSeconds $StartupTimeoutSeconds

    foreach ($path in @("http://127.0.0.1:$chatPort/health", "http://127.0.0.1:$chatPort/ready", "http://127.0.0.1:$chatPort/live")) {
        $ops = Invoke-WebRequest -Uri $path -UseBasicParsing
        if ($ops.StatusCode -ne 200) {
            throw "reference-chat-server ops endpoint $path returned status $($ops.StatusCode)"
        }
    }
    Write-Evidence "reference-chat-server: health/ready/live OK"

    $rooms = Invoke-WebRequest -Uri "http://127.0.0.1:$chatPort/rooms" -UseBasicParsing
    if ($rooms.StatusCode -ne 200) {
        throw "reference-chat-server /rooms returned status $($rooms.StatusCode)"
    }
    Write-Evidence "reference-chat-server: /rooms OK"

    $unauthSocket = Connect-WebSocket -Uri "ws://127.0.0.1:$chatPort/ws"
    $unauthFirst = Receive-WebSocketText -Socket $unauthSocket
    if ($unauthFirst -notmatch '"type"\s*:\s*"error"' -or $unauthFirst -notmatch '"code"\s*:\s*"auth_failed"') {
        throw "reference-chat-server unauthenticated WebSocket did not return auth_failed error"
    }
    Write-Evidence "reference-chat-server: unauthenticated websocket auth_failed OK"
    $unauthSocket.Dispose()
    $unauthSocket = $null

    $login = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$chatPort/login" -ContentType "application/json" -Body '{"username":"alice"}'
    if (-not $login.token) {
        throw "reference-chat-server login did not return a token"
    }

    $authSocket = Connect-WebSocket -Uri "ws://127.0.0.1:$chatPort/ws?token=$($login.token)"
    $welcome = Receive-WebSocketText -Socket $authSocket
    if ($welcome -notmatch '"type"\s*:\s*"welcome"') {
        throw "reference-chat-server authenticated WebSocket did not return welcome frame"
    }
    $roomList = Receive-WebSocketText -Socket $authSocket
    if ($roomList -notmatch '"type"\s*:\s*"room_list"') {
        throw "reference-chat-server authenticated WebSocket did not return room_list frame"
    }

    Send-WebSocketText -Socket $authSocket -Text '{"type":"join","room":"general"}'
    $history = Receive-WebSocketText -Socket $authSocket
    if ($history -notmatch '"type"\s*:\s*"history"') {
        throw "reference-chat-server join did not return history frame"
    }
    $joined = Receive-WebSocketText -Socket $authSocket
    if ($joined -notmatch '"type"\s*:\s*"joined"' -or $joined -notmatch '"room"\s*:\s*"general"') {
        throw "reference-chat-server join did not return joined frame for general room"
    }

    Send-WebSocketText -Socket $authSocket -Text '{"type":"chat","room":"general","message":"hello realtime"}'
    $message = Receive-WebSocketText -Socket $authSocket
    if ($message -notmatch '"type"\s*:\s*"message"' -or $message -notmatch '"message"\s*:\s*"hello realtime"') {
        throw "reference-chat-server chat did not echo message frame"
    }

    Write-Evidence "reference-chat-server: auth + websocket flow OK"
    $authSocket.Dispose()
    $authSocket = $null
    Stop-ExampleProcess -Process $chatProcess
    $chatProcess = $null

    Write-Host "[m387-smoke] Verifying streaming-demo SSE/request-boundary surface..."
    $streamPort = 3312
    $streamProcess = Start-ExampleProcess -ExePath $streamExe -Environment @{
        "STREAMING_DEMO_ADDR" = "127.0.0.1:$streamPort"
    }
    Wait-TcpPort -Address "127.0.0.1" -Port $streamPort -TimeoutSeconds $StartupTimeoutSeconds

    foreach ($path in @("http://127.0.0.1:$streamPort/health", "http://127.0.0.1:$streamPort/ready", "http://127.0.0.1:$streamPort/live")) {
        $ops = Invoke-WebRequest -Uri $path -UseBasicParsing
        if ($ops.StatusCode -ne 200) {
            throw "streaming-demo ops endpoint $path returned status $($ops.StatusCode)"
        }
    }
    Write-Evidence "streaming-demo: health/ready/live OK"

    $batch = Invoke-WebRequest -Method Post -Uri "http://127.0.0.1:$streamPort/api/chat" -ContentType "application/json" -Body '{"message":"hello world"}' -UseBasicParsing
    if ($batch.StatusCode -ne 200) {
        throw "streaming-demo POST /api/chat returned status $($batch.StatusCode)"
    }
    if ($batch.Content -notmatch '"reply"') {
        throw "streaming-demo POST /api/chat did not return JSON reply"
    }
    if (-not $batch.Headers["x-request-id"]) {
        throw "streaming-demo POST /api/chat did not return x-request-id header"
    }
    Write-Evidence "streaming-demo: batch JSON path OK"

    $stream = Invoke-WebRequest -Method Post -Uri "http://127.0.0.1:$streamPort/api/chat/stream" -ContentType "application/json" -Body '{"message":"hello world"}' -UseBasicParsing
    if ($stream.StatusCode -ne 200) {
        throw "streaming-demo POST /api/chat/stream returned status $($stream.StatusCode)"
    }
    if ($stream.Headers["Content-Type"] -notmatch "text/event-stream") {
        throw "streaming-demo SSE endpoint did not return text/event-stream"
    }
    if (-not $stream.Headers["x-request-id"]) {
        throw "streaming-demo SSE endpoint did not return x-request-id header"
    }
    if ($stream.Content -notmatch "data:" -or $stream.Content -notmatch "\[DONE\]") {
        throw "streaming-demo SSE endpoint did not emit expected frames"
    }

    Write-Evidence "streaming-demo: SSE request-boundary flow OK"
    Write-Host "Evidence: $EvidencePath"
}
finally {
    Pop-Location
    if ($unauthSocket) { $unauthSocket.Dispose() }
    if ($authSocket) { $authSocket.Dispose() }
    Stop-ExampleProcess -Process $chatProcess
    Stop-ExampleProcess -Process $streamProcess
}
