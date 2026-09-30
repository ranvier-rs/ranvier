param([string]$EvidenceDir = 'D:\ranvier-m427-jaeger-safety-20260930', [switch]$Tempo, [switch]$Datadog)
$ErrorActionPreference='Stop'
New-Item -ItemType Directory -Force $EvidenceDir | Out-Null
$config=Join-Path $EvidenceDir 'config.yaml'
'receivers: {}' | Set-Content $config
function global:Start-Sleep { param($Seconds) }
function global:Invoke-RestMethod { param($Uri,$Method,$TimeoutSec); if($TimeoutSec -ne 10){throw 'HTTP timeout missing'}; if($Uri -match '/api/traces') { if($global:jaegerCase -eq 'missing-trace') { @{data=@()} } else { @{data=@(@{spans=@(@{operationName='CalculationCircuit'},@{operationName='Circuit'},@{operationName='Node'},@{operationName='Node'})})} }; return }; if($global:jaegerCase -eq 'missing-service'){@{data=@();tagValues=@()}}else{@{data=@('otel-demo');tagValues=@('otel-demo')}} }
function global:cargo { $global:LASTEXITCODE=0; 'mock'; 'OTLP_SMOKE_EXPORTED outcome=12' }
function global:podman {
    $a=@($args); $global:jaegerCalls.Add(($a -join ' ')); $global:LASTEXITCODE=0
    if($a[0] -eq 'container' -and $a[1] -eq 'inspect') { 'd'*64; return }
    if($a[0] -eq 'container' -or ($a[0] -eq 'network' -and $a[1] -eq 'exists')) {
        $global:LASTEXITCODE=if($global:jaegerCase -eq 'present'){0}elseif($global:jaegerCase -eq 'query-error'){125}else{1}; return
    }
    if($a[0] -eq 'network' -and $a[1] -eq 'create') {
        if($global:jaegerCase -eq 'network-error'){$global:LASTEXITCODE=125}else{$a[2]}; return
    }
    if($a[0] -eq 'network' -and $a[1] -eq 'inspect') { if($global:jaegerCase -eq 'network-inspect-error'){$global:LASTEXITCODE=125}else{'c'*64}; return }
    if($a[0] -eq 'run') {
        $global:jaegerRuns++
        if($global:jaegerCase -eq 'second-start' -and $global:jaegerRuns -eq 2){$global:LASTEXITCODE=125}
        elseif($global:jaegerCase -eq 'invalid-id'){'bad'}
        else{if($global:jaegerRuns -eq 1){'a'*64}else{'b'*64}}; return
    }
    if($a[0] -eq 'logs') {
        if($global:jaegerCase -in @('logs-error','logs-cleanup-error')){$global:LASTEXITCODE=125}elseif($global:jaegerCase -eq 'backend-missing' -and $a[1] -eq ('a'*64)){'empty'}elseif($global:jaegerCase -eq 'missing-service'){'empty'}else{'service.name: Str(otel-demo)';'ResourceSpans'; if($global:jaegerCase -eq 'export-error'){'Exporting failed'}}; return
    }
    if($a[0] -eq 'rm') {if($global:jaegerCase -in @('cleanup-error','logs-cleanup-error')){throw 'mock cleanup failure'};return}
    if($a[0] -eq 'network' -and $a[1] -eq 'rm'){return}
    throw 'Unexpected command'
}
$fixture = if($Datadog){'../otel_collector_datadog_class_smoke_podman.ps1'}elseif($Tempo){'../otel_collector_tempo_smoke_podman.ps1'}else{'../otel_collector_jaeger_smoke_podman.ps1'}
$configArgs = if($Datadog){@{EdgeConfigPath=$config;BackendConfigPath=$config}}elseif($Tempo){@{CollectorConfigPath=$config;TempoConfigPath=$config}}else{@{ConfigPath=$config}}
try {
    $cases = @('present','query-error','network-error','network-inspect-error','second-start','invalid-id','logs-error','cleanup-error','logs-cleanup-error','missing-service','success')
    if($Datadog){$cases += @('backend-missing','export-error')}
    if(-not $Datadog -and -not $Tempo){$cases += 'missing-trace'}
    foreach($case in $cases) {
        $global:jaegerCase=$case; $global:jaegerCalls=[Collections.Generic.List[string]]::new(); $global:jaegerRuns=0
        $failure=$null
        try{ & (Join-Path $PSScriptRoot $fixture) @configArgs -EvidenceDir (Join-Path $EvidenceDir $case) -StartupWaitSeconds 0 }catch{$failure=$_}
        if(($case -eq 'success') -eq [bool]$failure){throw "Wrong outcome: $case"}
        $count=if($case -eq 'second-start'){1}elseif($case -in @('logs-error','cleanup-error','logs-cleanup-error','missing-service','success','backend-missing','export-error','missing-trace')){2}else{0}
        if(@($global:jaegerCalls|Where-Object {$_ -like 'rm *'}).Count -ne $count){throw "Wrong container cleanup: $case"}
        $network=$case -notin @('present','query-error','network-error','network-inspect-error')
        if(@($global:jaegerCalls|Where-Object {$_ -like 'network rm *'}).Count -ne [int]$network){throw "Wrong network cleanup: $case"}
        if($case -eq 'logs-cleanup-error' -and $failure.Exception.Message -ne ($(if($Datadog){'Edge collector logs failed.'}else{'Collector logs failed.'}))){throw 'Primary error lost'}
        foreach($call in @($global:jaegerCalls|Where-Object {$_ -like 'rm *' -or $_ -like 'network rm *'})){if($call -notmatch '[a-c]{64}$'){throw 'Unowned cleanup'}}
        Write-Host "PASS: $case"
    }
} finally { Remove-Item Function:\podman,Function:\cargo,Function:\Start-Sleep,Function:\Invoke-RestMethod }
Write-Host ('PASS: ' + $cases.Count + ' mocked backend cases; no live runtime executed.')
