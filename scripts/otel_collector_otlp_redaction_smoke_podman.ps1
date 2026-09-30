param(
    [string]$ContainerName = ("ranvier-otel-redaction-smoke-" + [Guid]::NewGuid().ToString("N")),
    [ValidateRange(0, 300)][int]$StartupWaitSeconds = 4,
    [string]$EvidenceDir = "",
    [ValidateRange(1,65535)][int]$GrpcPort = 4317,
    [ValidateRange(1,65535)][int]$HttpPort = 4318,
    [string]$ConfigPath = "",
    [string]$CollectorImage = "docker.io/otel/opentelemetry-collector-contrib:latest",
    [ValidateSet("grpc", "http/protobuf")]
    [string]$Protocol = "grpc",
    [ValidateSet("off", "public", "strict")][string]$RedactMode = "public"
)

$ErrorActionPreference = "Stop"

$PSNativeCommandUseErrorActionPreference = $false
$ownedId = $null
$primaryFailure = $null
$savedEnvironment = @{}
foreach ($name in @('RANVIER_OTLP_ENDPOINT', 'RANVIER_OTLP_PROTOCOL', 'RUST_LOG', 'RANVIER_TELEMETRY_REDACT_MODE', 'RANVIER_TELEMETRY_REDACT_KEYS')) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
}
if (-not (Get-Command podman -ErrorAction SilentlyContinue)) {
    throw "podman is not installed or not in PATH."
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ranvierRoot = (Resolve-Path (Join-Path $scriptDir "..")).Path
$workspaceRoot = (Resolve-Path (Join-Path $ranvierRoot "..")).Path

if ([string]::IsNullOrWhiteSpace($ConfigPath)) {
    $defaultConfig = Join-Path $workspaceRoot "docs/03_guides/otel_collector_smoke_config.yaml"
    if (-not (Test-Path $defaultConfig)) {
        throw "Collector config not found. Provide -ConfigPath or restore docs/03_guides/otel_collector_smoke_config.yaml."
    }
    $configPath = (Resolve-Path $defaultConfig).Path
} else {
    if (-not (Test-Path $ConfigPath)) {
        throw "Provided -ConfigPath does not exist: $ConfigPath"
    }
    $configPath = (Resolve-Path $ConfigPath).Path
}
if ([string]::IsNullOrWhiteSpace($EvidenceDir)) {
    $EvidenceDir = Join-Path ([IO.Path]::GetTempPath()) ('ranvier-otel-' + [Guid]::NewGuid().ToString('N'))
}
$evidenceDir = [IO.Path]::GetFullPath($EvidenceDir)
New-Item -ItemType Directory -Path $evidenceDir -Force | Out-Null

$stamp = Get-Date -Format "yyyyMMdd_HHmmss"
$appLog = Join-Path $evidenceDir ("otel_demo_redaction_smoke_app_" + $stamp + ".log")
$collectorLog = Join-Path $evidenceDir ("otel_demo_redaction_smoke_collector_" + $stamp + ".log")

try {
    podman container exists $ContainerName | Out-Null
    $queryExit = $LASTEXITCODE
    if ($queryExit -eq 0) { throw 'Container name already exists; existing resource preserved.' }
    if ($queryExit -ne 1) { throw "Container query failed ($queryExit); existing resources preserved." }

    $created = @(podman run -d --rm --name $ContainerName `
        -p "127.0.0.1:${GrpcPort}:4317" `
        -p "127.0.0.1:${HttpPort}:4318" `
        -v "${configPath}:/etc/otelcol-contrib/config.yaml" `
        $CollectorImage)
    if ($LASTEXITCODE -ne 0) { throw 'Collector creation failed.' }
    $createdId = ($created -join '').Trim()
    if ($createdId -notmatch '^[a-f0-9]{64}$') { throw 'Invalid owned container ID; resource preserved.' }
    $ownedId = $createdId
    $imageId = (podman container inspect $ownedId --format '{{.Image}}' | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $imageId -notmatch '^(sha256:)?[a-f0-9]{64}$') { throw 'Collector image identity query failed.' }
    [ordered]@{ container_id=$ownedId; image_id=$imageId; config_sha256=(Get-FileHash -LiteralPath $configPath).Hash } |
        ConvertTo-Json | Set-Content -LiteralPath (Join-Path $evidenceDir 'collector-runtime-identity.json') -Encoding utf8

    Start-Sleep -Seconds $StartupWaitSeconds

    Push-Location $ranvierRoot
    try {
        $otlpEndpoint = if ($Protocol -eq "http/protobuf") {
            "http://localhost:$HttpPort"
        } else {
            "http://localhost:$GrpcPort"
        }

        $env:RANVIER_OTLP_ENDPOINT = $otlpEndpoint
        $env:RANVIER_OTLP_PROTOCOL = $Protocol
        $env:RANVIER_TELEMETRY_REDACT_MODE = $RedactMode
        $env:RANVIER_TELEMETRY_REDACT_KEYS = "email,api_key"
        $env:RUST_LOG = "info"

        cargo run --locked -p ranvier-observe --example otlp_smoke --features otlp *>&1 | Tee-Object -FilePath $appLog | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "OTel demo failed." }
    } finally {
        Pop-Location
    }

    Start-Sleep -Seconds 3
    podman logs $ownedId *> $collectorLog
    if ($LASTEXITCODE -ne 0) { throw "Collector log capture failed." }

    foreach ($required in @('ResourceSpans', 'ResourceMetrics', 'ranvier_demo_runs_total')) {
        if (-not (Select-String -LiteralPath $collectorLog -SimpleMatch $required)) { throw "Collector receipt missing: $required" }
    }
    if (-not (Select-String -LiteralPath $appLog -SimpleMatch 'OTLP_SMOKE_EXPORTED outcome=12')) { throw 'Finite export success marker missing.' }
    if ($RedactMode -eq 'strict') {
        foreach ($forbidden in @('CalculationCircuit', 'service.name: Str(otel-demo)', 'outcome: Str(next)')) {
            if (Select-String -LiteralPath $collectorLog -SimpleMatch $forbidden) { throw 'Strict identity or label value leaked.' }
        }
    }

    $redactedEmail = Select-String -Path $collectorLog -Pattern "customer_email: Str([REDACTED])" -SimpleMatch
    $redactedApiKey = Select-String -Path $collectorLog -Pattern "api_key: Str([REDACTED])" -SimpleMatch
    $rawEmailLeak = Select-String -Path $collectorLog -Pattern "demo.user@example.com" -SimpleMatch
    $rawApiKeyLeak = Select-String -Path $collectorLog -Pattern "demo-api-key-123" -SimpleMatch
    $appErrorMatch = Select-String -Path $appLog -Pattern "OpenTelemetry trace error" -SimpleMatch

    if ($RedactMode -eq "off") {
        if (-not (Select-String -Path $collectorLog -Pattern "demo.user@example.com" -SimpleMatch)) {
            throw "Expected raw email attribute in collector log when RedactMode=off."
        }
        if (-not $rawApiKeyLeak) { throw "Expected raw api_key attribute when RedactMode=off." }
    } else {
        if (-not $redactedEmail) {
            throw "Collector log does not contain redacted customer_email attribute."
        }

        if (-not $redactedApiKey) {
            throw "Collector log does not contain redacted api_key attribute."
        }

        if ($rawEmailLeak) {
            throw "Collector log still contains raw customer_email value."
        }

        if ($rawApiKeyLeak) {
            throw "Collector log still contains raw api_key value."
        }
    }

    if (Select-String -Path $appLog -Pattern "OpenTelemetry metrics error" -SimpleMatch) { throw "App log contains OpenTelemetry metrics error." }
    if ($appErrorMatch) {
        throw "App log contains OpenTelemetry trace error."
    }

} catch {
    $primaryFailure = $_
    throw
} finally {
    foreach ($name in $savedEnvironment.Keys) {
        if ($null -eq $savedEnvironment[$name]) { Remove-Item "Env:$name" -ErrorAction SilentlyContinue }
        else { [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name]) }
    }
    if ($ownedId) {
        try {
            podman rm -f $ownedId | Out-Null
            if ($LASTEXITCODE -ne 0) { throw 'Owned collector cleanup failed.' }
        } catch {
            if ($primaryFailure) { Write-Warning $_.Exception.Message }
            else { throw }
        }
    }
}
Write-Host 'OTel OTLP redaction smoke passed; owned collector removed.'
Write-Host "APP_LOG=$appLog"
Write-Host "COLLECTOR_LOG=$collectorLog"
