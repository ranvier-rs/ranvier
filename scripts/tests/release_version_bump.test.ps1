# Exercises prepare_release_version_bump.ps1 on a throwaway two-crate workspace:
# dry run leaves files alone, -Apply edits manifests and the current-version JSON
# fields only, and previous-version mentions are classified by
# release_version_mentions.json. Needs pwsh, git and cargo (offline metadata only).
$ErrorActionPreference = 'Stop'
$scriptsDir = Resolve-Path (Join-Path $PSScriptRoot '..')
$root = Join-Path ([IO.Path]::GetTempPath()) ("ranvier-bump-test-" + [Guid]::NewGuid().ToString('N'))
$utf8 = New-Object System.Text.UTF8Encoding($false)
$failures = New-Object System.Collections.Generic.List[string]

function Assert-That([bool]$Condition, [string]$Message) {
    if ($Condition) { Write-Host "ok   $Message" } else { Write-Host "FAIL $Message"; $failures.Add($Message) }
}
function Write-Fixture([string]$Relative, [string]$Text) {
    $path = Join-Path $root $Relative
    New-Item -ItemType Directory -Force -Path (Split-Path $path) | Out-Null
    [IO.File]::WriteAllText($path, $Text, $utf8)
}
function Read-Fixture([string]$Relative) { [IO.File]::ReadAllText((Join-Path $root $Relative)) }
function Get-TreeHash {
    (Get-ChildItem -Path $root -Recurse -File | Where-Object { $_.FullName -notmatch '[\\/](\.git|target|evidence)[\\/]' } |
        Sort-Object FullName | ForEach-Object { (Get-FileHash $_.FullName).Hash }) -join ''
}
function Invoke-Bump([string]$Target, [switch]$Apply) {
    $evidence = Join-Path $root ("evidence/" + [Guid]::NewGuid().ToString('N'))
    $arguments = @('-NoProfile', '-File', (Join-Path $root 'scripts/prepare_release_version_bump.ps1'), '-TargetVersion', $Target, '-EvidenceDir', $evidence)
    if ($Apply) { $arguments += '-Apply' }
    Push-Location $root
    try { $output = & pwsh @arguments 2>&1 } finally { Pop-Location }
    if ($LASTEXITCODE -ne 0) { throw "bump script failed: $output" }
    return Get-Content -Raw (Get-ChildItem -Path $evidence -Filter '*.json' | Select-Object -First 1).FullName | ConvertFrom-Json
}

try {
    Write-Fixture 'Cargo.toml' "[workspace]`nmembers = [`"alpha`", `"beta`"]`nresolver = `"2`"`n`n[workspace.package]`nversion = `"1.4.0`"`nedition = `"2021`"`n`n[workspace.dependencies]`nranvier-alpha = { path = `"alpha`", version = `"1.4.0`" }`n"
    Write-Fixture 'alpha/Cargo.toml' "[package]`nname = `"ranvier-alpha`"`nversion.workspace = true`nedition.workspace = true`n"
    Write-Fixture 'alpha/src/lib.rs' "pub fn alpha() {}`n"
    Write-Fixture 'beta/Cargo.toml' "[package]`nname = `"ranvier-beta`"`nversion = `"1.4.0`"`nedition = `"2021`"`n`n[dependencies]`nranvier-alpha = { path = `"../alpha`", version = `"1.4.0`" }`n"
    Write-Fixture 'beta/src/lib.rs' "pub fn beta() {}`n"
    Write-Fixture '.ranvier-api-policy.json' "{`n  `"schema_version`": `"1.0.0`",`n  `"product_version`": `"1.4.0`",`n  `"crates`": []`n}`n"
    Write-Fixture '.ranvier-observe-api-policy.json' "{`r`n  `"crates`": [],`r`n  `"product_version`": `"1.4.0`"`r`n}`r`n"
    Write-Fixture '.ranvier-examples-manifest.json' "{`n  `"version`": `"1.4.0`",`n  `"examples`": [`n    { `"name`": `"x`", `"version`": `"1.4.0`" }`n  ]`n}`n"
    Write-Fixture 'examples/catalog.json' "{`n  `"workspace_version`": `"1.3.0`"`n}`n"
    Write-Fixture 'README.md' "Current release v1.4.0. Older: 11.4.0 and 1.40.`n"
    Write-Fixture 'docs/CHANGELOG.md' "## [1.4.0]`n"
    Write-Fixture 'notes.txt' "pinned to 1.4`n"
    New-Item -ItemType Directory -Force -Path (Join-Path $root 'scripts') | Out-Null
    foreach ($file in @('prepare_release_version_bump.ps1', 'release_common.ps1', 'release_version_mentions.json')) {
        Copy-Item (Join-Path $scriptsDir $file) (Join-Path $root "scripts/$file") -Force
    }
    & git -C $root init -q
    & git -C $root add -A
    if ($LASTEXITCODE -ne 0) { throw 'git add failed' }

    # 1. Dry run: nothing changes, but the plan and the report describe the bumped tree.
    $before = Get-TreeHash
    $plan = Invoke-Bump -Target '1.5.0'
    Assert-That ((Get-TreeHash) -eq $before) 'dry run leaves every file unchanged'
    Assert-That ($plan.previous_version -eq '1.4.0') 'previous_version is read from workspace.package'
    $fields = @{}; foreach ($f in $plan.version_fields) { $fields[$f.path] = $f.status }
    Assert-That ($fields['.ranvier-api-policy.json'] -eq 'updated') 'api policy product_version planned'
    Assert-That ($fields['.ranvier-observe-api-policy.json'] -eq 'updated') 'observe policy product_version planned'
    Assert-That ($fields['.ranvier-examples-manifest.json'] -eq 'updated') 'examples manifest version planned'
    Assert-That ($fields['examples/catalog.json'] -eq 'unexpected-value') 'catalog with another version is reported, not changed'
    Assert-That (@($plan.warnings | Where-Object { $_ -like '*catalog.json*unexpected-value*' }).Count -eq 1) 'unexpected value appears in warnings'
    $mentions = $plan.previous_version_mentions
    Assert-That ($mentions.searched) 'mention search ran'
    Assert-That (@($mentions.by_category.release_docs | Where-Object { $_ -like 'README.md:1:*' }).Count -eq 1) 'README mention classified as release_docs'
    Assert-That (@($mentions.by_category.historical | Where-Object { $_ -like 'docs/CHANGELOG.md:*' }).Count -eq 1) 'CHANGELOG mention classified as historical'
    # The nested example entry keeps its value and has no rule, so it is reported.
    $unclassified = @($mentions.unclassified)
    Assert-That ($unclassified.Count -eq 2 -and $unclassified[0] -like '.ranvier-examples-manifest.json:4:*' -and $unclassified[1] -like 'notes.txt:1:*') "unclassified = nested manifest entry and notes.txt (got: $($unclassified -join ' | '))"
    $allLines = $unclassified + @($mentions.by_category.PSObject.Properties | ForEach-Object { $_.Value })
    Assert-That (@($allLines | Where-Object { $_ -like '*Cargo.toml*' -or $_ -like '*policy.json*' -or $_ -like '.ranvier-examples-manifest.json:2:*' }).Count -eq 0) 'lines this run edits are not reported as remaining'
    Assert-That (@($mentions.next_steps).Count -ge 3) 'next steps are listed'

    # 2. Apply: manifests and the planned fields change; formatting is kept.
    $observeBefore = Read-Fixture '.ranvier-observe-api-policy.json'
    $applied = Invoke-Bump -Target '1.5.0' -Apply
    Assert-That ((Read-Fixture 'Cargo.toml') -match 'version = "1\.5\.0"' -and (Read-Fixture 'Cargo.toml') -notmatch '1\.4\.0') 'workspace manifest bumped'
    Assert-That ((Read-Fixture 'beta/Cargo.toml') -notmatch '1\.4\.0') 'member manifest and dependency bumped'
    Assert-That ((Read-Fixture '.ranvier-api-policy.json') -match '"product_version": "1\.5\.0"') 'api policy updated'
    $observeAfter = Read-Fixture '.ranvier-observe-api-policy.json'
    Assert-That ($observeAfter -eq $observeBefore.Replace('"1.4.0"', '"1.5.0"')) 'observe policy keeps CRLF and changes only the version'
    $manifest = Read-Fixture '.ranvier-examples-manifest.json'
    Assert-That ($manifest -match '(?m)^  "version": "1\.5\.0"' -and $manifest -match '"name": "x", "version": "1\.4\.0"') 'only the top-level manifest version changes'
    Assert-That ((Read-Fixture 'examples/catalog.json') -match '1\.3\.0') 'unexpected catalog value left alone'
    $bytes = [IO.File]::ReadAllBytes((Join-Path $root '.ranvier-api-policy.json'))
    Assert-That (-not ($bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB)) 'no BOM is added'
    Assert-That (@($applied.previous_version_mentions.unclassified).Count -eq 2) 'apply reports the same unclassified mentions'

    # 3. Same target again: nothing to do, nothing searched.
    $again = Invoke-Bump -Target '1.5.0'
    Assert-That ($again.changed_file_count -eq 0 -and -not $again.previous_version_mentions.searched) 'rerun at the target version is a no-op'
}
finally {
    Remove-Item -Recurse -Force -LiteralPath $root -ErrorAction SilentlyContinue
}

if ($failures.Count -gt 0) {
    Write-Host "$($failures.Count) assertion(s) failed"
    exit 1
}
Write-Host 'release_version_bump.test.ps1: all assertions passed'
