param(
    [string]$NetworkName = ("ranvier-otel-tempo-net-" + [Guid]::NewGuid().ToString("N")),
    [string]$CollectorContainerName = ("ranvier-otel-collector-tempo-" + [Guid]::NewGuid().ToString("N")),
    [string]$TempoContainerName = ("ranvier-otel-tempo-" + [Guid]::NewGuid().ToString("N")),
    [ValidateRange(0,300)][int]$StartupWaitSeconds = 22,
    [string]$EvidenceDir = "",
    [string]$CollectorConfigPath = "",
    [string]$TempoConfigPath = ""
)

$ErrorActionPreference = "Stop"

$PSNativeCommandUseErrorActionPreference = $false
$ownedIds = [Collections.Generic.List[string]]::new()
$ownedNetworkId = $null
$primaryFailure = $null
$savedEnvironment = @{}
foreach ($name in @('RANVIER_OTLP_ENDPOINT','RANVIER_OTLP_PROTOCOL','RUST_LOG')) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
}
function Assert-NewFixture {
    param([string]$Kind, [string]$Name)
    podman $Kind exists $Name | Out-Null
    $code = $LASTEXITCODE
    if ($code -eq 0) { throw "Existing $Kind preserved: $Name" }
    if ($code -ne 1) { throw "Fixture query failed ($code): $Name" }
}
function Get-OwnedId {
    param([object[]]$Output, [int]$Code)
    if ($Code -ne 0) { throw 'Fixture creation failed.' }
    $id = ($Output -join '').Trim()
    if ($id -notmatch '^[a-f0-9]{64}$') { throw 'Invalid owned fixture ID; resource preserved.' }
    return $id
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

if ([string]::IsNullOrWhiteSpace($EvidenceDir)) {
    $EvidenceDir = Join-Path ([IO.Path]::GetTempPath()) ('ranvier-tempo-' + [Guid]::NewGuid().ToString('N'))
}
$evidenceDir = [IO.Path]::GetFullPath($EvidenceDir)
New-Item -ItemType Directory -Path $evidenceDir -Force | Out-Null

$stamp = Get-Date -Format "yyyyMMdd_HHmmss"
$appLog = Join-Path $evidenceDir ("otel_demo_tempo_smoke_app_" + $stamp + ".log")
$collectorLog = Join-Path $evidenceDir ("otel_demo_tempo_smoke_collector_" + $stamp + ".log")
$tempoLog = Join-Path $evidenceDir ("otel_demo_tempo_smoke_tempo_" + $stamp + ".log")
$tagValuesLog = Join-Path $evidenceDir ("otel_demo_tempo_smoke_tag_values_" + $stamp + ".json")



try {
    Assert-NewFixture container $CollectorContainerName
    Assert-NewFixture container $TempoContainerName
    Assert-NewFixture network $NetworkName
    $created = @(podman network create $NetworkName)
    if ($LASTEXITCODE -ne 0) { throw 'Network creation failed.' }
    # Podman network create prints the name; obtain its immutable ID separately.
    $networkIdentity = @(podman network inspect $NetworkName --format '{{.ID}}')
    $ownedNetworkId = Get-OwnedId $networkIdentity $LASTEXITCODE
     $created = @(podman run -d --rm --name $TempoContainerName `
        --network $ownedNetworkId `
        --network-alias ranvier-otel-tempo `
        -p 127.0.0.1:3200:3200 `
        -v "${tempoConfig}:/etc/tempo.yaml" `
        docker.io/grafana/tempo:2.6.1 `
        '--config.file=/etc/tempo.yaml')
    $ownedIds.Add((Get-OwnedId $created $LASTEXITCODE))

     $created = @(podman run -d --rm --name $CollectorContainerName `
        --network $ownedNetworkId `
        -p 127.0.0.1:4317:4317 `
        -p 127.0.0.1:4318:4318 `
        -v "${collectorConfig}:/etc/otelcol-contrib/config.yaml" `
        docker.io/otel/opentelemetry-collector-contrib:latest)
    $ownedIds.Add((Get-OwnedId $created $LASTEXITCODE))

    Start-Sleep -Seconds $StartupWaitSeconds

    Push-Location $ranvierRoot
    try {
        $env:RANVIER_OTLP_ENDPOINT = "http://localhost:4317"
        $env:RANVIER_OTLP_PROTOCOL = "grpc"
        $env:RUST_LOG = "info"

        cargo run --locked -p otel-demo *>&1 | Tee-Object -FilePath $appLog | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "OTel demo failed." }
    } finally {
        Pop-Location
    }

    Start-Sleep -Seconds 12
    podman logs $ownedIds[1] *> $collectorLog
    if ($LASTEXITCODE -ne 0) { throw "Collector logs failed." }
    podman logs $ownedIds[0] *> $tempoLog
    if ($LASTEXITCODE -ne 0) { throw "Tempo logs failed." }

    $serviceMatch = Select-String -Path $collectorLog -Pattern "service.name: Str(otel-demo)" -SimpleMatch
    $spansMatch = Select-String -Path $collectorLog -Pattern "ResourceSpans" -SimpleMatch
    $collectorExportErrors = Select-String -Path $collectorLog -Pattern "Exporting failed|A record lookup error|rpc error" -CaseSensitive:$false
    $appErrorMatch = Select-String -Path $appLog -Pattern "OpenTelemetry trace error" -SimpleMatch

    $tagValuesResponse = $null
    $tagValues = @()

    for ($i = 0; $i -lt 6; $i++) {
        try {
            $tagValuesResponse = Invoke-RestMethod -Uri "http://localhost:3200/api/search/tag/service.name/values" -Method GET -TimeoutSec 10
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

} catch {
    $primaryFailure = $_
    throw
} finally {
    foreach ($name in $savedEnvironment.Keys) {
        if ($null -eq $savedEnvironment[$name]) { Remove-Item "Env:$name" -ErrorAction SilentlyContinue }
        else { [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name]) }
    }
    $cleanupFailures = [Collections.Generic.List[string]]::new()
    foreach ($id in $ownedIds) {
        try {
            podman rm -f $id | Out-Null
            if ($LASTEXITCODE -ne 0) { throw "Container cleanup failed: $id" }
        } catch { $cleanupFailures.Add($_.Exception.Message) }
    }
    if ($ownedNetworkId) {
        try {
            podman network rm $ownedNetworkId | Out-Null
            if ($LASTEXITCODE -ne 0) { throw 'Owned network cleanup failed.' }
        } catch { $cleanupFailures.Add($_.Exception.Message) }
    }
    if ($cleanupFailures.Count) {
        if ($primaryFailure) { Write-Warning ($cleanupFailures -join '; ') }
        else { throw ($cleanupFailures -join '; ') }
    }
}
Write-Host 'OTel Tempo smoke passed; owned fixtures removed.'
Write-Host "APP_LOG=$appLog"
Write-Host "COLLECTOR_LOG=$collectorLog"
Write-Host "TEMPO_LOG=$tempoLog"
Write-Host "TAG_VALUES_LOG=$tagValuesLog"