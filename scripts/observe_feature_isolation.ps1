param([Parameter(Mandatory)][string]$EvidenceDir)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$destination = [IO.Path]::GetFullPath($EvidenceDir)
if (Test-Path -LiteralPath $destination) { throw 'Evidence directory already exists.' }
[IO.Directory]::CreateDirectory($destination) | Out-Null
Push-Location $root
try {
    $source = (& git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Source identity query failed.' }
    $compiler = (& rustc --version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Compiler identity query failed.' }
    $graphs = @{}
    foreach ($case in @('ranvier-observe', 'ranvier-core', 'ranvier-core-default', 'ranvier-observe-default')) {
        $package = $case.Replace('-default', '')
        $featureArgs = @(if (-not $case.EndsWith('-default')) { '--no-default-features' })
        $output = @(& cargo tree -p $package @featureArgs --edges normal --prefix none --locked --offline 2>&1)
        $code = $LASTEXITCODE
        $output | Set-Content -LiteralPath (Join-Path $destination "$case-tree.log") -Encoding utf8
        if ($code -ne 0) { throw "Dependency query failed: $package" }
        $graphs[$case] = $output
    }
    if ($graphs['ranvier-observe'].Count -ne 1 -or $graphs['ranvier-observe'][0] -notmatch '^ranvier-observe v') {
        throw 'Default observe adapter unexpectedly has normal dependencies.'
    }
    if ($graphs['ranvier-core'] -match '^(opentelemetry(?:[-_][\w-]+)?|tonic(?:[-_][\w-]+)?|reqwest|tower(?:-http)?) v') {
        throw 'Core normal dependency graph includes an OTLP transport dependency.'
    }
    if ($graphs['ranvier-observe-default'].Count -ne 1) { throw 'Default observe features add normal dependencies.' }
    if ($graphs['ranvier-core-default'] -match '^(opentelemetry(?:[-_][\w-]+)?|tonic(?:[-_][\w-]+)?|reqwest|tower(?:-http)?) v') {
        throw 'Default core features include an OTLP transport dependency.'
    }
    [ordered]@{
        source = $source; compiler = $compiler
        cargo_lock_sha256 = (Get-FileHash -LiteralPath (Join-Path $root 'Cargo.lock')).Hash
        status = 'PASS'; scope = 'observe/core normal dependency graphs with and without default features'
    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $destination 'result.json') -Encoding utf8
} finally { Pop-Location }
Write-Host 'Observe/core normal dependency isolation PASS.'
