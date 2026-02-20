param(
    [string]$NetworkName = "ranvier-otel-net",
    [string]$CollectorContainerName = "ranvier-otel-collector",
    [string]$JaegerContainerName = "ranvier-otel-jaeger",
    [int]$StartupWaitSeconds = 8,
    [string]$ConfigPath = ""
)

$ErrorActionPreference = "Stop"

function Stop-ContainerIfExists {
    param([string]$Name)

    $existing = podman ps -a --format "{{.Names}}" | Select-String -Pattern "^$Name$"
    if ($existing) {
        podman rm -f $Name | Out-Null
    }
}

function Ensure-Network {
    param([string]$Name)

    $existing = podman network ls --format "{{.Name}}" | Select-String -Pattern "^$Name$"
    if (-not $existing) {
        podman network create $Name | Out-Null
        return $true
    }

    return $false
}

if (-not (Get-Command podman -ErrorAction SilentlyContinue)) {
    throw "podman is not installed or not in PATH."
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ranvierRoot = (Resolve-Path (Join-Path $scriptDir "..")).Path
$workspaceRoot = (Resolve-Path (Join-Path $ranvierRoot "..")).Path

if ([string]::IsNullOrWhiteSpace($ConfigPath)) {
    $defaultConfig = Join-Path $workspaceRoot "docs/03_guides/otel_collector_jaeger_config.yaml"
    if (-not (Test-Path $defaultConfig)) {
        throw "Collector config not found. Provide -ConfigPath or restore docs/03_guides/otel_collector_jaeger_config.yaml."
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
$appLog = Join-Path $evidenceDir ("otel_demo_jaeger_smoke_app_" + $stamp + ".log")
$collectorLog = Join-Path $evidenceDir ("otel_demo_jaeger_smoke_collector_" + $stamp + ".log")
$jaegerLog = Join-Path $evidenceDir ("otel_demo_jaeger_smoke_jaeger_" + $stamp + ".log")
$servicesLog = Join-Path $evidenceDir ("otel_demo_jaeger_smoke_services_" + $stamp + ".log")

$createdNetwork = $false

try {
    Stop-ContainerIfExists -Name $CollectorContainerName
    Stop-ContainerIfExists -Name $JaegerContainerName

    $createdNetwork = Ensure-Network -Name $NetworkName

    podman run -d --name $JaegerContainerName `
        --network $NetworkName `
        -p 16686:16686 `
        -e COLLECTOR_OTLP_ENABLED=true `
        jaegertracing/all-in-one:1.60.0 | Out-Null

    podman run -d --name $CollectorContainerName `
        --network $NetworkName `
        -p 4317:4317 `
        -p 4318:4318 `
        -v "${configPath}:/etc/otelcol-contrib/config.yaml" `
        otel/opentelemetry-collector-contrib:latest | Out-Null

    Start-Sleep -Seconds $StartupWaitSeconds

    Push-Location $ranvierRoot
    try {
        $env:RANVIER_OTLP_ENDPOINT = "http://localhost:4317"
        $env:RANVIER_OTLP_PROTOCOL = "grpc"
        $env:RUST_LOG = "info"

        cargo run -p otel-demo *>&1 | Tee-Object -FilePath $appLog | Out-Null
    } finally {
        Pop-Location
    }

    Start-Sleep -Seconds 5
    podman logs $CollectorContainerName *> $collectorLog
    podman logs $JaegerContainerName *> $jaegerLog

    $serviceMatch = Select-String -Path $collectorLog -Pattern "service.name: Str(otel-demo)" -SimpleMatch
    $spansMatch = Select-String -Path $collectorLog -Pattern "ResourceSpans" -SimpleMatch
    $appErrorMatch = Select-String -Path $appLog -Pattern "OpenTelemetry trace error" -SimpleMatch

    if (-not $serviceMatch) {
        throw "Collector log does not contain service.name=otel-demo."
    }

    if (-not $spansMatch) {
        throw "Collector log does not contain ResourceSpans."
    }

    if ($appErrorMatch) {
        throw "App log contains OpenTelemetry trace error."
    }

    $servicesResponse = Invoke-RestMethod -Uri "http://localhost:16686/api/services" -Method GET
    $services = @($servicesResponse.data)
    ("SERVICES=" + ($services -join ",")) | Set-Content -Path $servicesLog -Encoding UTF8

    if ($services -notcontains "otel-demo") {
        throw "Jaeger service list does not include otel-demo."
    }

    Write-Host "OTel Jaeger smoke passed."
    Write-Host "APP_LOG=$appLog"
    Write-Host "COLLECTOR_LOG=$collectorLog"
    Write-Host "JAEGER_LOG=$jaegerLog"
    Write-Host "SERVICES_LOG=$servicesLog"
} finally {
    Stop-ContainerIfExists -Name $CollectorContainerName
    Stop-ContainerIfExists -Name $JaegerContainerName

    if ($createdNetwork) {
        podman network rm $NetworkName | Out-Null
    }
}
