$ErrorActionPreference='Stop'
function global:podman {
    $a=@($args); $global:depCalls.Add(($a -join ' ')); $global:LASTEXITCODE=0
    switch($a[0]) {
        'container' { $global:LASTEXITCODE=if($global:depCase -eq 'present'){0}elseif($global:depCase -eq 'query-error'){125}else{1} }
        'run' { $global:depRuns++; if($global:depCase -eq 'second-start' -and $global:depRuns -eq 2){$global:LASTEXITCODE=125}elseif($global:depCase -eq 'invalid-id'){'bad'}else{if($global:depRuns -eq 1){'a'*64}else{'b'*64}} }
        'exec' { 'PONG' }
        'rm' { if($global:depCase -eq 'cargo-cleanup-error'){throw 'mock cleanup error'} }
        default { throw 'Unexpected command' }
    }
}
function global:cargo { $global:LASTEXITCODE=1; 'mock build failure' }
$listeners = @([Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0),[Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0))
foreach ($listener in $listeners) { $listener.Start() }
$fixturePorts = @{PostgresPort=$listeners[0].LocalEndpoint.Port; RedisPort=$listeners[1].LocalEndpoint.Port}
try {
    $prior = Join-Path ([IO.Path]::GetTempPath()) ('ranvier-prior-' + [Guid]::NewGuid().ToString('N') + '.md')
    'preserved evidence' | Set-Content $prior
    $global:depCalls = [Collections.Generic.List[string]]::new()
    $rejected = $false
    try { & (Join-Path $PSScriptRoot '../dependency_failure_smoke_podman.ps1') -SummaryPath $prior }
    catch { if ($_.Exception.Message -notlike 'Summary path already exists*') { throw }; $rejected = $true }
    if (-not $rejected -or $global:depCalls.Count -or (Get-Content -Raw $prior).Trim() -ne 'preserved evidence') { throw 'Prior evidence preservation failed' }
    Remove-Item -LiteralPath $prior
    Write-Host 'PASS: prior summary preserved before Podman launch'

    foreach($case in @('present','query-error','second-start','invalid-id','cargo-error','cargo-cleanup-error')) {
        $global:depCase=$case; $global:depRuns=0; $global:depCalls=[Collections.Generic.List[string]]::new()
        $failed=$null
        try{ & (Join-Path $PSScriptRoot '../dependency_failure_smoke_podman.ps1') @fixturePorts }catch{$failed=$_}
        if(-not $failed){throw 'Expected rejection'}
        $expected=if($case -eq 'second-start'){1}elseif($case -in @('cargo-error','cargo-cleanup-error')){2}else{0}
        if(@($global:depCalls|Where-Object {$_ -like 'rm *'}).Count -ne $expected){throw "Wrong cleanup count: $case"}
        if($case -eq 'cargo-cleanup-error' -and $failed.Exception.Message -notlike 'dependency recovery probe build failed*'){throw 'Primary error lost'}
        Write-Host "PASS: $case"
    }
} finally { foreach($listener in $listeners){$listener.Stop()}; Remove-Item Function:\podman,Function:\cargo }
Write-Host 'PASS: 7 mocked evidence/startup/build controls; no live recovery accepted.'