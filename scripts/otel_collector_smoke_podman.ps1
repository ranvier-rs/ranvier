param(
    [string]$ContainerName = "ranvier-otel-smoke",
    [int]$StartupWaitSeconds = 4,
    [string]$ConfigPath = "",
    [ValidateSet("grpc", "http/protobuf")]
    [string]$Protocol = "grpc"
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
    $configPath = (Resolve-Path $defaultConfig).Path
} else {
    if (-not (Test-Path $ConfigPath)) {
        throw "Provided -ConfigPath does not exist: $ConfigPath"
    }
    $configPath = (Resolve-Path $ConfigPath).Path
}
$evidenceDir = Join-Path $workspaceRoot "docs/05_dev_plans/evidence"
New-Item -ItemType Directory -Path $evidenceDir -Force | Out-Null

$stamp = Get-Date -Format "yyyyMMdd_HHmmss"
$appLog = Join-Path $evidenceDir ("otel_demo_smoke_app_" + $stamp + ".log")
$collectorLog = Join-Path $evidenceDir ("otel_demo_smoke_collector_" + $stamp + ".log")

try {
    Stop-ContainerIfExists -Name $ContainerName

    podman run -d --name $ContainerName `
        -p 4317:4317 `
        -p 4318:4318 `
        -v "${configPath}:/etc/otelcol-contrib/config.yaml" `
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
        $env:RUST_LOG = "info"

        cargo run -p otel-demo *>&1 | Tee-Object -FilePath $appLog | Out-Null
    } finally {
        Pop-Location
    }

    Start-Sleep -Seconds 3
    podman logs $ContainerName *> $collectorLog

    $serviceMatch = Select-String -Path $collectorLog -Pattern "service.name: Str(otel-demo)" -SimpleMatch
    $spansMatch = Select-String -Path $collectorLog -Pattern "ResourceSpans" -SimpleMatch
    $metricsMatch = Select-String -Path $collectorLog -Pattern "ResourceMetrics" -SimpleMatch
    $appErrorMatch = Select-String -Path $appLog -Pattern "OpenTelemetry trace error" -SimpleMatch
    $appMetricErrorMatch = Select-String -Path $appLog -Pattern "OpenTelemetry metrics error" -SimpleMatch

    if (-not $serviceMatch) {
        throw "Collector log does not contain service.name=otel-demo."
    }

    if (-not $spansMatch) {
        throw "Collector log does not contain ResourceSpans."
    }

    if (-not $metricsMatch) {
        throw "Collector log does not contain ResourceMetrics."
    }

    if ($appErrorMatch) {
        throw "App log contains OpenTelemetry trace error."
    }

    if ($appMetricErrorMatch) {
        throw "App log contains OpenTelemetry metrics error."
    }

    Write-Host "OTel smoke passed."
    Write-Host "APP_LOG=$appLog"
    Write-Host "COLLECTOR_LOG=$collectorLog"
} finally {
    Stop-ContainerIfExists -Name $ContainerName
}
