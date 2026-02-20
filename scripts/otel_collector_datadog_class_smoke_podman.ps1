param(
    [string]$NetworkName = "ranvier-otel-dd-net",
    [string]$EdgeContainerName = "ranvier-otel-dd-edge",
    [string]$BackendContainerName = "ranvier-otel-dd-backend",
    [int]$StartupWaitSeconds = 10,
    [string]$EdgeConfigPath = "",
    [string]$BackendConfigPath = ""
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

function Resolve-ConfigPath {
    param(
        [string]$ProvidedPath,
        [string]$DefaultPath,
        [string]$Label
    )

    if ([string]::IsNullOrWhiteSpace($ProvidedPath)) {
        if (-not (Test-Path $DefaultPath)) {
            throw "$Label config not found. Provide path explicitly or restore: $DefaultPath"
        }
        return (Resolve-Path $DefaultPath).Path
    }

    if (-not (Test-Path $ProvidedPath)) {
        throw "Provided $Label config path does not exist: $ProvidedPath"
    }

    return (Resolve-Path $ProvidedPath).Path
}

if (-not (Get-Command podman -ErrorAction SilentlyContinue)) {
    throw "podman is not installed or not in PATH."
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ranvierRoot = (Resolve-Path (Join-Path $scriptDir "..")).Path
$workspaceRoot = (Resolve-Path (Join-Path $ranvierRoot "..")).Path

$edgeConfig = Resolve-ConfigPath `
    -ProvidedPath $EdgeConfigPath `
    -DefaultPath (Join-Path $workspaceRoot "docs/03_guides/otel_collector_datadog_class_edge_config.yaml") `
    -Label "Edge collector"
$backendConfig = Resolve-ConfigPath `
    -ProvidedPath $BackendConfigPath `
    -DefaultPath (Join-Path $workspaceRoot "docs/03_guides/otel_collector_datadog_class_backend_config.yaml") `
    -Label "Backend collector"

$evidenceDir = Join-Path $workspaceRoot "docs/05_dev_plans/evidence"
New-Item -ItemType Directory -Path $evidenceDir -Force | Out-Null

$stamp = Get-Date -Format "yyyyMMdd_HHmmss"
$appLog = Join-Path $evidenceDir ("otel_demo_datadog_class_smoke_app_" + $stamp + ".log")
$edgeCollectorLog = Join-Path $evidenceDir ("otel_demo_datadog_class_smoke_edge_collector_" + $stamp + ".log")
$backendCollectorLog = Join-Path $evidenceDir ("otel_demo_datadog_class_smoke_backend_collector_" + $stamp + ".log")

$createdNetwork = $false

try {
    Stop-ContainerIfExists -Name $EdgeContainerName
    Stop-ContainerIfExists -Name $BackendContainerName

    $createdNetwork = Ensure-Network -Name $NetworkName

    podman run -d --name $BackendContainerName `
        --network $NetworkName `
        -v "${backendConfig}:/etc/otelcol-contrib/config.yaml" `
        otel/opentelemetry-collector-contrib:latest | Out-Null

    podman run -d --name $EdgeContainerName `
        --network $NetworkName `
        -p 4317:4317 `
        -p 4318:4318 `
        -v "${edgeConfig}:/etc/otelcol-contrib/config.yaml" `
        otel/opentelemetry-collector-contrib:latest | Out-Null

    Start-Sleep -Seconds $StartupWaitSeconds

    Push-Location $ranvierRoot
    try {
        $env:RANVIER_OTLP_ENDPOINT = "http://localhost:4318"
        $env:RANVIER_OTLP_PROTOCOL = "http/protobuf"
        $env:RUST_LOG = "info"

        cargo run -p otel-demo *>&1 | Tee-Object -FilePath $appLog | Out-Null
    } finally {
        Pop-Location
    }

    Start-Sleep -Seconds 6

    podman logs $EdgeContainerName *> $edgeCollectorLog
    podman logs $BackendContainerName *> $backendCollectorLog

    $edgeServiceMatch = Select-String -Path $edgeCollectorLog -Pattern "service.name: Str(otel-demo)" -SimpleMatch
    $edgeSpansMatch = Select-String -Path $edgeCollectorLog -Pattern "ResourceSpans" -SimpleMatch
    $backendServiceMatch = Select-String -Path $backendCollectorLog -Pattern "service.name: Str(otel-demo)" -SimpleMatch
    $backendSpansMatch = Select-String -Path $backendCollectorLog -Pattern "ResourceSpans" -SimpleMatch
    $edgeExportErrors = Select-String -Path $edgeCollectorLog -Pattern "Exporting failed|rpc error|connection refused|context deadline exceeded" -CaseSensitive:$false
    $appErrorMatch = Select-String -Path $appLog -Pattern "OpenTelemetry trace error" -SimpleMatch

    if (-not $edgeServiceMatch) {
        throw "Edge collector log does not contain service.name=otel-demo."
    }

    if (-not $edgeSpansMatch) {
        throw "Edge collector log does not contain ResourceSpans."
    }

    if (-not $backendServiceMatch) {
        throw "Backend collector log does not contain service.name=otel-demo."
    }

    if (-not $backendSpansMatch) {
        throw "Backend collector log does not contain ResourceSpans."
    }

    if ($edgeExportErrors) {
        throw "Edge collector log contains OTLP forwarding export errors."
    }

    if ($appErrorMatch) {
        throw "App log contains OpenTelemetry trace error."
    }

    Write-Host "OTel Datadog-class smoke passed."
    Write-Host "APP_LOG=$appLog"
    Write-Host "EDGE_COLLECTOR_LOG=$edgeCollectorLog"
    Write-Host "BACKEND_COLLECTOR_LOG=$backendCollectorLog"
} finally {
    Stop-ContainerIfExists -Name $EdgeContainerName
    Stop-ContainerIfExists -Name $BackendContainerName

    if ($createdNetwork) {
        podman network rm $NetworkName | Out-Null
    }
}
