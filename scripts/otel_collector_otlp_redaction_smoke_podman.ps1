param(
    [string]$ContainerName = "ranvier-otel-redaction-smoke",
    [int]$StartupWaitSeconds = 4,
    [string]$ConfigPath = "",
    [ValidateSet("grpc", "http/protobuf")]
    [string]$Protocol = "grpc",
    [ValidateSet("off", "public", "strict")]
    [string]$RedactMode = "public"
)

$ErrorActionPreference = "Stop"

function Stop-ContainerIfExists {
    param([string]$Name)

    $existing = podman ps -a --format "{{.Names}}" | Select-String -Pattern "^$Name$"
    if ($existing) {
        podman rm -f $Name | Out-Null
    }
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
    $collectorConfig = (Resolve-Path $defaultConfig).Path
} else {
    if (-not (Test-Path $ConfigPath)) {
        throw "Provided -ConfigPath does not exist: $ConfigPath"
    }
    $collectorConfig = (Resolve-Path $ConfigPath).Path
}

$evidenceDir = Join-Path $workspaceRoot "docs/05_dev_plans/evidence"
New-Item -ItemType Directory -Path $evidenceDir -Force | Out-Null

$stamp = Get-Date -Format "yyyyMMdd_HHmmss"
$appLog = Join-Path $evidenceDir ("otel_demo_redaction_smoke_app_" + $stamp + ".log")
$collectorLog = Join-Path $evidenceDir ("otel_demo_redaction_smoke_collector_" + $stamp + ".log")

try {
    Stop-ContainerIfExists -Name $ContainerName

    podman run -d --name $ContainerName `
        -p 4317:4317 `
        -p 4318:4318 `
        -v "${collectorConfig}:/etc/otelcol-contrib/config.yaml" `
        otel/opentelemetry-collector-contrib:latest | Out-Null

    Start-Sleep -Seconds $StartupWaitSeconds

    Push-Location $ranvierRoot
    try {
        $otlpEndpoint = if ($Protocol -eq "http/protobuf") {
            "http://localhost:4318"
        } else {
            "http://localhost:4317"
        }

        $env:RANVIER_OTLP_ENDPOINT = $otlpEndpoint
        $env:RANVIER_OTLP_PROTOCOL = $Protocol
        $env:RANVIER_TELEMETRY_REDACT_MODE = $RedactMode
        $env:RANVIER_TELEMETRY_REDACT_KEYS = "email,api_key"
        $env:RUST_LOG = "info"

        cargo run -p otel-demo *>&1 | Tee-Object -FilePath $appLog | Out-Null
    } finally {
        Pop-Location
    }

    Start-Sleep -Seconds 4
    podman logs $ContainerName *> $collectorLog

    $redactedEmail = Select-String -Path $collectorLog -Pattern "customer_email: Str([REDACTED])" -SimpleMatch
    $redactedApiKey = Select-String -Path $collectorLog -Pattern "api_key: Str([REDACTED])" -SimpleMatch
    $rawEmailLeak = Select-String -Path $collectorLog -Pattern "demo.user@example.com" -SimpleMatch
    $rawApiKeyLeak = Select-String -Path $collectorLog -Pattern "demo-api-key-123" -SimpleMatch
    $appErrorMatch = Select-String -Path $appLog -Pattern "OpenTelemetry trace error" -SimpleMatch

    if ($RedactMode -eq "off") {
        if (-not (Select-String -Path $collectorLog -Pattern "demo.user@example.com" -SimpleMatch)) {
            throw "Expected raw email attribute in collector log when RedactMode=off."
        }
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

    if ($appErrorMatch) {
        throw "App log contains OpenTelemetry trace error."
    }

    Write-Host "OTel OTLP redaction smoke passed."
    Write-Host "APP_LOG=$appLog"
    Write-Host "COLLECTOR_LOG=$collectorLog"
} finally {
    Stop-ContainerIfExists -Name $ContainerName
}
