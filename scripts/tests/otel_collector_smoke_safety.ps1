param([string]$EvidenceDir = 'D:\ranvier-m427-otel-safety-20260930')
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $EvidenceDir | Out-Null
$config = Join-Path $EvidenceDir 'config.yaml'
'receivers: {}' | Set-Content $config
$fixture = Join-Path $PSScriptRoot '../otel_collector_smoke_podman.ps1'
$original = @{}
foreach ($name in @('RANVIER_OTLP_ENDPOINT','RANVIER_OTLP_PROTOCOL','RUST_LOG')) { $original[$name] = [Environment]::GetEnvironmentVariable($name) }
function global:Start-Sleep { param($Seconds) }
function global:podman {
    $call = @($args)
    $global:otelCalls.Add(($call -join ' '))
    $global:LASTEXITCODE = 0
    switch ($call[0]) {
        'container' { $global:LASTEXITCODE = if($global:otelCase -eq 'present'){0}elseif($global:otelCase -eq 'query-error'){125}else{1} }
        'run' { if($global:otelCase -eq 'run-error'){$global:LASTEXITCODE=125}elseif($global:otelCase -eq 'invalid-id'){'bad'}else{'a'*64} }
        'logs' { if($global:otelCase -eq 'logs-error'){$global:LASTEXITCODE=125}else{'service.name: Str(otel-demo)'; 'ResourceSpans'; 'ResourceMetrics'} }
        'rm' { if($global:otelCase -eq 'cargo-and-cleanup-throw'){throw 'mock cleanup invocation error'}; if($global:otelCase -in @('cleanup-error','cargo-and-cleanup-error')){$global:LASTEXITCODE=125} }
        default { throw 'Unexpected Podman command' }
    }
}
function global:cargo {
    $global:LASTEXITCODE = if($global:otelCase -in @('cargo-error','cargo-and-cleanup-error','cargo-and-cleanup-throw')){1}else{0}
    'mock demo'
}
try {
    foreach ($case in @('present','query-error','run-error','invalid-id','cargo-error','logs-error','cleanup-error','cargo-and-cleanup-error','cargo-and-cleanup-throw','success')) {
        $global:otelCase=$case
        $global:otelCalls=[Collections.Generic.List[string]]::new()
        $failed=$null
        try { & $fixture -ConfigPath $config -EvidenceDir (Join-Path $EvidenceDir $case) -StartupWaitSeconds 0 } catch {$failed=$_}
        if(($case -eq 'success') -eq [bool]$failed){throw "Unexpected outcome: $case"}
        $removed=@($global:otelCalls|Where-Object {$_ -like 'rm *'})
        $owns=$case -in @('cargo-error','logs-error','cleanup-error','cargo-and-cleanup-error','cargo-and-cleanup-throw','success')
        if($removed.Count -ne [int]$owns){throw "Invalid cleanup count: $case"}
        if($owns -and $removed[0] -ne ('rm -f '+('a'*64))){throw 'Cleanup used an unowned identifier'}
        if($case -in @('cargo-and-cleanup-error','cargo-and-cleanup-throw') -and $failed.Exception.Message -ne 'OTel demo failed.'){throw 'Primary error replaced'}
        foreach($name in $original.Keys){if([Environment]::GetEnvironmentVariable($name) -cne $original[$name]){throw "Environment leaked: $name"}}
        Write-Host "PASS: $case"
    }
} finally {
    foreach($name in $original.Keys){[Environment]::SetEnvironmentVariable($name,$original[$name])}
    Remove-Item Function:\podman,Function:\cargo,Function:\Start-Sleep
}
Write-Host 'PASS: 10 mocked collector ownership cases; no live runtime executed.'