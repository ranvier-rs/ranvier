param(
    [string]$NetworkName = "ranvier-otel-tempo-net",
    [string]$CollectorContainerName = "ranvier-otel-collector-tempo",
    [string]$TempoContainerName = "ranvier-otel-tempo",
    [int]$StartupWaitSeconds = 22,
    [string]$CollectorConfigPath = "",
    [string]$TempoConfigPath = ""
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

$collectorConfig = Resolve-ConfigPath `
    -ProvidedPath $CollectorConfigPath `
    -DefaultPath (Join-Path $workspaceRoot "docs/03_guides/otel_collector_tempo_config.yaml") `
    -Label "Collector"
$tempoConfig = Resolve-ConfigPath `
    -ProvidedPath $TempoConfigPath `
    -DefaultPath (Join-Path $workspaceRoot "docs/03_guides/tempo_smoke_config.yaml") `
    -Label "Tempo"

$evidenceDir = Join-Path $workspaceRoot "docs/05_dev_plans/evidence"
New-Item -ItemType Directory -Path $evidenceDir -Force | Out-Null

$stamp = Get-Date -Format "yyyyMMdd_HHmmss"
$appLog = Join-Path $evidenceDir ("otel_demo_tempo_smoke_app_" + $stamp + ".log")
$collectorLog = Join-Path $evidenceDir ("otel_demo_tempo_smoke_collector_" + $stamp + ".log")
$tempoLog = Join-Path $evidenceDir ("otel_demo_tempo_smoke_tempo_" + $stamp + ".log")
$tagValuesLog = Join-Path $evidenceDir ("otel_demo_tempo_smoke_tag_values_" + $stamp + ".json")

$createdNetwork = $false

try {
    Stop-ContainerIfExists -Name $CollectorContainerName
    Stop-ContainerIfExists -Name $TempoContainerName

    $createdNetwork = Ensure-Network -Name $NetworkName

    podman run -d --name $TempoContainerName `
        --network $NetworkName `
        -p 3200:3200 `
        -v "${tempoConfig}:/etc/tempo.yaml" `
        grafana/tempo:2.6.1 `
        '--config.file=/etc/tempo.yaml' | Out-Null

    podman run -d --name $CollectorContainerName `
        --network $NetworkName `
        -p 4317:4317 `
        -p 4318:4318 `
        -v "${collectorConfig}:/etc/otelcol-contrib/config.yaml" `
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

    Start-Sleep -Seconds 12

    podman logs $CollectorContainerName *> $collectorLog
    podman logs $TempoContainerName *> $tempoLog

    $serviceMatch = Select-String -Path $collectorLog -Pattern "service.name: Str(otel-demo)" -SimpleMatch
    $spansMatch = Select-String -Path $collectorLog -Pattern "ResourceSpans" -SimpleMatch
    $collectorExportErrors = Select-String -Path $collectorLog -Pattern "Exporting failed|A record lookup error|rpc error" -CaseSensitive:$false
    $appErrorMatch = Select-String -Path $appLog -Pattern "OpenTelemetry trace error" -SimpleMatch

    $tagValuesResponse = $null
    $tagValues = @()

    for ($i = 0; $i -lt 6; $i++) {
        try {
            $tagValuesResponse = Invoke-RestMethod -Uri "http://localhost:3200/api/search/tag/service.name/values" -Method GET
            $tagValues = @($tagValuesResponse.tagValues)
            if ($tagValues -contains "otel-demo") {
                break
            }
        } catch {
            # Tempo API can transiently return not-ready while ingester settles.
        }
        Start-Sleep -Seconds 3
    }

    if ($tagValuesResponse) {
        $tagValuesResponse | ConvertTo-Json -Depth 8 | Set-Content -Path $tagValuesLog -Encoding UTF8
    } else {
        "{}" | Set-Content -Path $tagValuesLog -Encoding UTF8
    }

    if (-not $serviceMatch) {
        throw "Collector log does not contain service.name=otel-demo."
    }

    if (-not $spansMatch) {
        throw "Collector log does not contain ResourceSpans."
    }

    if ($collectorExportErrors) {
        throw "Collector log contains export errors for Tempo forwarding path."
    }

    if ($appErrorMatch) {
        throw "App log contains OpenTelemetry trace error."
    }

    if ($tagValues -notcontains "otel-demo") {
        throw "Tempo search API did not report service.name value 'otel-demo'."
    }

    Write-Host "OTel Tempo smoke passed."
    Write-Host "APP_LOG=$appLog"
    Write-Host "COLLECTOR_LOG=$collectorLog"
    Write-Host "TEMPO_LOG=$tempoLog"
    Write-Host "TAG_VALUES_LOG=$tagValuesLog"
} finally {
    Stop-ContainerIfExists -Name $CollectorContainerName
    Stop-ContainerIfExists -Name $TempoContainerName

    if ($createdNetwork) {
        podman network rm $NetworkName | Out-Null
    }
}
