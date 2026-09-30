param(
    [string]$ContainerName = ("ranvier-otel-smoke-" + [Guid]::NewGuid().ToString("N")),
    [ValidateRange(0, 300)][int]$StartupWaitSeconds = 4,
    [string]$EvidenceDir = "",
    [string]$ConfigPath = "",
    [string]$CollectorImage = "docker.io/otel/opentelemetry-collector-contrib:latest",
    [ValidateSet("grpc", "http/protobuf")]
    [string]$Protocol = "grpc"
)

$ErrorActionPreference = "Stop"

$PSNativeCommandUseErrorActionPreference = $false
$ownedId = $null
$primaryFailure = $null
$savedEnvironment = @{}
foreach ($name in @('RANVIER_OTLP_ENDPOINT', 'RANVIER_OTLP_PROTOCOL', 'RUST_LOG')) {
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
$appLog = Join-Path $evidenceDir ("otel_demo_smoke_app_" + $stamp + ".log")
$collectorLog = Join-Path $evidenceDir ("otel_demo_smoke_collector_" + $stamp + ".log")

try {
    podman container exists $ContainerName | Out-Null
    $queryExit = $LASTEXITCODE
    if ($queryExit -eq 0) { throw 'Container name already exists; existing resource preserved.' }
    if ($queryExit -ne 1) { throw "Container query failed ($queryExit); existing resources preserved." }

    $created = @(podman run -d --rm --name $ContainerName `
        -p 127.0.0.1:4317:4317 `
        -p 127.0.0.1:4318:4318 `
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
            "http://localhost:4318"
        } else {
            "http://localhost:4317"
        }

        $env:RANVIER_OTLP_ENDPOINT = $otlpEndpoint
        $env:RANVIER_OTLP_PROTOCOL = $Protocol
        $env:RUST_LOG = "info"

        cargo run --locked -p otel-demo *>&1 | Tee-Object -FilePath $appLog | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "OTel demo failed." }
    } finally {
        Pop-Location
    }

    Start-Sleep -Seconds 3
    podman logs $ownedId *> $collectorLog
    if ($LASTEXITCODE -ne 0) { throw "Collector log capture failed." }

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
Write-Host 'OTel smoke passed; owned collector removed.'
Write-Host "APP_LOG=$appLog"
Write-Host "COLLECTOR_LOG=$collectorLog"
