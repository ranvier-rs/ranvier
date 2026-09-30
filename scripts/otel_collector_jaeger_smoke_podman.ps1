param(
    [string]$NetworkName = ("ranvier-otel-net-" + [Guid]::NewGuid().ToString("N")),
    [string]$CollectorContainerName = ("ranvier-otel-collector-" + [Guid]::NewGuid().ToString("N")),
    [string]$JaegerContainerName = ("ranvier-otel-jaeger-" + [Guid]::NewGuid().ToString("N")),
    [ValidateRange(0, 300)][int]$StartupWaitSeconds = 8,
    [string]$EvidenceDir = "",
    [string]$ConfigPath = "",
    [ValidateRange(1,65535)][int]$GrpcPort = 4317,
    [ValidateRange(1,65535)][int]$HttpPort = 4318,
    [ValidateRange(1,65535)][int]$JaegerPort = 16686,
    [string]$CollectorImage = 'docker.io/otel/opentelemetry-collector-contrib:latest',
    [string]$JaegerImage = 'docker.io/jaegertracing/all-in-one:1.60.0'
)

$ErrorActionPreference = "Stop"

$PSNativeCommandUseErrorActionPreference = $false
$ownedIds = [Collections.Generic.List[string]]::new()
$ownedNetworkId = $null
$primaryFailure = $null
$savedEnvironment = @{}
foreach ($name in @('RANVIER_OTLP_ENDPOINT','RANVIER_OTLP_PROTOCOL','RUST_LOG','RANVIER_TELEMETRY_REDACT_MODE','RANVIER_TELEMETRY_REDACT_KEYS')) {
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

if ([string]::IsNullOrWhiteSpace($EvidenceDir)) {
    $EvidenceDir = Join-Path ([IO.Path]::GetTempPath()) ('ranvier-jaeger-' + [Guid]::NewGuid().ToString('N'))
}
$evidenceDir = [IO.Path]::GetFullPath($EvidenceDir)
New-Item -ItemType Directory -Path $evidenceDir -Force | Out-Null

$stamp = Get-Date -Format "yyyyMMdd_HHmmss"
$appLog = Join-Path $evidenceDir ("otel_demo_jaeger_smoke_app_" + $stamp + ".log")
$collectorLog = Join-Path $evidenceDir ("otel_demo_jaeger_smoke_collector_" + $stamp + ".log")
$jaegerLog = Join-Path $evidenceDir ("otel_demo_jaeger_smoke_jaeger_" + $stamp + ".log")
$servicesLog = Join-Path $evidenceDir ("otel_demo_jaeger_smoke_services_" + $stamp + ".log")



try {
    Assert-NewFixture container $CollectorContainerName
    Assert-NewFixture container $JaegerContainerName
    Assert-NewFixture network $NetworkName
    $created = @(podman network create $NetworkName)
    if ($LASTEXITCODE -ne 0) { throw 'Network creation failed.' }
    # Podman network create prints the name; obtain its immutable ID separately.
    $networkIdentity = @(podman network inspect $NetworkName --format '{{.ID}}')
    $ownedNetworkId = Get-OwnedId $networkIdentity $LASTEXITCODE
     $created = @(podman run -d --rm --name $JaegerContainerName `
        --network $ownedNetworkId `
        --network-alias ranvier-otel-jaeger `
        -p "127.0.0.1:${JaegerPort}:16686" `
        -e COLLECTOR_OTLP_ENABLED=true `
        $JaegerImage)
    $ownedIds.Add((Get-OwnedId $created $LASTEXITCODE))

     $created = @(podman run -d --rm --name $CollectorContainerName `
        --network $ownedNetworkId `
        -p "127.0.0.1:${GrpcPort}:4317" `
        -p "127.0.0.1:${HttpPort}:4318" `
        -v "${configPath}:/etc/otelcol-contrib/config.yaml" `
        $CollectorImage)
    $ownedIds.Add((Get-OwnedId $created $LASTEXITCODE))

    Start-Sleep -Seconds $StartupWaitSeconds

    Push-Location $ranvierRoot
    try {
        $env:RANVIER_OTLP_ENDPOINT = "http://127.0.0.1:$GrpcPort"
        $env:RANVIER_OTLP_PROTOCOL = "grpc"
        $env:RUST_LOG = "info"
        $env:RANVIER_TELEMETRY_REDACT_MODE = 'public'
        $env:RANVIER_TELEMETRY_REDACT_KEYS = ''

        cargo run --locked -p ranvier-observe --example otlp_smoke --features otlp *>&1 | Tee-Object -FilePath $appLog | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "OTel demo failed." }
    } finally {
        Pop-Location
    }

    Start-Sleep -Seconds 5
    podman logs $ownedIds[1] *> $collectorLog
    if ($LASTEXITCODE -ne 0) { throw "Collector logs failed." }
    podman logs $ownedIds[0] *> $jaegerLog
    if ($LASTEXITCODE -ne 0) { throw "Jaeger logs failed." }

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

    if (-not (Select-String -LiteralPath $appLog -SimpleMatch 'OTLP_SMOKE_EXPORTED outcome=12')) { throw 'Finite export marker missing.' }
    $servicesResponse = Invoke-RestMethod -Uri "http://127.0.0.1:$JaegerPort/api/services" -Method GET -TimeoutSec 10
    $services = @($servicesResponse.data)
    ("SERVICES=" + ($services -join ",")) | Set-Content -Path $servicesLog -Encoding UTF8

    if ($services -notcontains "otel-demo") {
        throw "Jaeger service list does not include otel-demo."
    }
    $tracesResponse = Invoke-RestMethod -Uri "http://127.0.0.1:$JaegerPort/api/traces?service=otel-demo&limit=10" -Method GET -TimeoutSec 10
    $tracesResponse | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath (Join-Path $evidenceDir 'backend-traces.json') -Encoding utf8
    $complete = @($tracesResponse.data | Where-Object {
        $names = @($_.spans.operationName)
        @($names | Where-Object { $_ -eq 'CalculationCircuit' }).Count -eq 1 -and
        @($names | Where-Object { $_ -eq 'Circuit' }).Count -eq 1 -and
        @($names | Where-Object { $_ -eq 'Node' }).Count -eq 2
    })
    if ($complete.Count -ne 1) { throw 'Jaeger runtime trace receipt missing or ambiguous.' }

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
Write-Host 'OTel Jaeger smoke passed; owned fixtures removed.'
Write-Host "APP_LOG=$appLog"
Write-Host "COLLECTOR_LOG=$collectorLog"
Write-Host "JAEGER_LOG=$jaegerLog"
Write-Host "SERVICES_LOG=$servicesLog"
