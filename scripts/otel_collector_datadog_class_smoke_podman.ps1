param(
    [string]$NetworkName = ("ranvier-otel-dd-net-" + [Guid]::NewGuid().ToString("N")),
    [string]$EdgeContainerName = ("ranvier-otel-dd-edge-" + [Guid]::NewGuid().ToString("N")),
    [string]$BackendContainerName = ("ranvier-otel-dd-backend-" + [Guid]::NewGuid().ToString("N")),
    [ValidateRange(0,300)][int]$StartupWaitSeconds = 10,
    [string]$EvidenceDir = "",
    [string]$EdgeConfigPath = "",
    [string]$BackendConfigPath = ""
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

$edgeConfig = Resolve-ConfigPath `
    -ProvidedPath $EdgeConfigPath `
    -DefaultPath (Join-Path $workspaceRoot "docs/03_guides/otel_collector_datadog_class_edge_config.yaml") `
    -Label "Edge collector"
$backendConfig = Resolve-ConfigPath `
    -ProvidedPath $BackendConfigPath `
    -DefaultPath (Join-Path $workspaceRoot "docs/03_guides/otel_collector_datadog_class_backend_config.yaml") `
    -Label "Backend collector"

if ([string]::IsNullOrWhiteSpace($EvidenceDir)) {
    $EvidenceDir = Join-Path ([IO.Path]::GetTempPath()) ('ranvier-tempo-' + [Guid]::NewGuid().ToString('N'))
}
$evidenceDir = [IO.Path]::GetFullPath($EvidenceDir)
New-Item -ItemType Directory -Path $evidenceDir -Force | Out-Null

$stamp = Get-Date -Format "yyyyMMdd_HHmmss"
$appLog = Join-Path $evidenceDir ("otel_demo_datadog_class_smoke_app_" + $stamp + ".log")
$edgeCollectorLog = Join-Path $evidenceDir ("otel_demo_datadog_class_smoke_edge_collector_" + $stamp + ".log")
$backendCollectorLog = Join-Path $evidenceDir ("otel_demo_datadog_class_smoke_backend_collector_" + $stamp + ".log")

try {
    Assert-NewFixture container $EdgeContainerName
    Assert-NewFixture container $BackendContainerName
    Assert-NewFixture network $NetworkName
    $created = @(podman network create $NetworkName)
    if ($LASTEXITCODE -ne 0) { throw 'Network creation failed.' }
    # Podman network create prints the name; obtain its immutable ID separately.
    $networkIdentity = @(podman network inspect $NetworkName --format '{{.ID}}')
    $ownedNetworkId = Get-OwnedId $networkIdentity $LASTEXITCODE
     $created = @(podman run -d --rm --name $BackendContainerName `
        --network $ownedNetworkId `
        --network-alias ranvier-otel-dd-backend `
        -v "${backendConfig}:/etc/otelcol-contrib/config.yaml" `
        docker.io/otel/opentelemetry-collector-contrib:latest)
    $ownedIds.Add((Get-OwnedId $created $LASTEXITCODE))

     $created = @(podman run -d --rm --name $EdgeContainerName `
        --network $ownedNetworkId `
        -p 127.0.0.1:4317:4317 `
        -p 127.0.0.1:4318:4318 `
        -v "${edgeConfig}:/etc/otelcol-contrib/config.yaml" `
        docker.io/otel/opentelemetry-collector-contrib:latest)
    $ownedIds.Add((Get-OwnedId $created $LASTEXITCODE))

    Start-Sleep -Seconds $StartupWaitSeconds

    Push-Location $ranvierRoot
    try {
        $env:RANVIER_OTLP_ENDPOINT = "http://localhost:4318"
        $env:RANVIER_OTLP_PROTOCOL = "http/protobuf"
        $env:RUST_LOG = "info"

        cargo run --locked -p otel-demo *>&1 | Tee-Object -FilePath $appLog | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "OTel demo failed." }
    } finally {
        Pop-Location
    }

    Start-Sleep -Seconds 6
    podman logs $ownedIds[1] *> $edgeCollectorLog
    if ($LASTEXITCODE -ne 0) { throw "Edge collector logs failed." }
    podman logs $ownedIds[0] *> $backendCollectorLog
    if ($LASTEXITCODE -ne 0) { throw "Backend collector logs failed." }

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
Write-Host 'OTel Datadog-class smoke passed; owned fixtures removed.'
Write-Host "APP_LOG=$appLog"
Write-Host "EDGE_COLLECTOR_LOG=$edgeCollectorLog"
Write-Host "BACKEND_COLLECTOR_LOG=$backendCollectorLog"