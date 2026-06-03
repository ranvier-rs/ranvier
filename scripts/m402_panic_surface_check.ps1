param(
    [string]$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path,
    [string]$AllowlistPath = (Join-Path $PSScriptRoot "m402_panic_surface_allowlist.json")
)

$ErrorActionPreference = "Stop"

$patterns = @(
    @{ Kind = "unwrap"; Regex = "\.unwrap\s*\(" },
    @{ Kind = "expect"; Regex = "\.expect\s*\(" },
    @{ Kind = "panic"; Regex = "panic!\s*\(" },
    @{ Kind = "todo"; Regex = "todo!\s*\(" },
    @{ Kind = "unimplemented"; Regex = "unimplemented!\s*\(" }
)

function ConvertTo-RepoPath {
    param([string]$Path)
    $fullPath = (Resolve-Path $Path).Path
    $rootPath = (Resolve-Path $Root).Path
    return $fullPath.Substring($rootPath.Length + 1).Replace("\", "/")
}

function Get-BraceDelta {
    param([string]$Line)
    $withoutStrings = [regex]::Replace($Line, '"([^"\\]|\\.)*"', '""')
    $opens = [regex]::Matches($withoutStrings, "\{").Count
    $closes = [regex]::Matches($withoutStrings, "\}").Count
    return $opens - $closes
}

function Get-RustFiles {
    $roots = @(
        "core/src",
        "runtime/src",
        "http/src",
        "guard/src",
        "std/src",
        "kit/src"
    )

    $extensionsRoot = Join-Path $Root "extensions"
    if (Test-Path $extensionsRoot) {
        Get-ChildItem -Path $extensionsRoot -Directory | ForEach-Object {
            $roots += "extensions/$($_.Name)/src"
        }
    }

    foreach ($relativeRoot in $roots) {
        $scanRoot = Join-Path $Root $relativeRoot
        if (!(Test-Path $scanRoot)) {
            continue
        }

        Get-ChildItem -Path $scanRoot -Recurse -Filter "*.rs" | Where-Object {
            $repoPath = (ConvertTo-RepoPath $_.FullName)
            $repoPath -notmatch '(^|/)(tests|examples|benches)/' -and
                $repoPath -notmatch '(^|/)src/(tests|test_harness|testkit)\.rs$'
        }
    }
}

function Get-ProductionHits {
    $hits = New-Object System.Collections.Generic.List[object]

    foreach ($file in Get-RustFiles) {
        $repoPath = ConvertTo-RepoPath $file.FullName
        $lines = Get-Content $file.FullName
        $skipDepth = 0
        $pendingTestBlock = $false

        for ($i = 0; $i -lt $lines.Count; $i++) {
            $line = $lines[$i]
            $trimmed = $line.Trim()

            if ($skipDepth -gt 0) {
                $skipDepth += Get-BraceDelta $line
                if ($skipDepth -le 0) {
                    $skipDepth = 0
                }
                continue
            }

            if ($pendingTestBlock) {
                if ($trimmed.Length -eq 0) {
                    continue
                }
                if ($trimmed -match '^\s*(pub(\([^)]+\))?\s+)?(mod|fn|async\s+fn)\s+') {
                    $pendingTestBlock = $false
                    $delta = Get-BraceDelta $line
                    $skipDepth = [Math]::Max($delta, 1)
                    continue
                }
                $pendingTestBlock = $false
            }

            if ($trimmed -match '^#\[(cfg\(test\)|test|tokio::test)') {
                $pendingTestBlock = $true
                continue
            }

            if ($trimmed -match '^(//|///|//!)') {
                continue
            }

            foreach ($pattern in $patterns) {
                if ($line -match $pattern.Regex) {
                    $contextEnd = [Math]::Min($i + 3, $lines.Count - 1)
                    $context = ($lines[$i..$contextEnd] -join " ").Trim()
                    $hits.Add([pscustomobject]@{
                        Path = $repoPath
                        Line = $i + 1
                        Kind = $pattern.Kind
                        Text = $trimmed
                        Context = $context
                    }) | Out-Null
                    break
                }
            }
        }
    }

    return $hits
}

$allowlist = @()
if (Test-Path $AllowlistPath) {
    $loaded = Get-Content $AllowlistPath -Raw | ConvertFrom-Json
    if ($loaded) {
        $allowlist = @($loaded)
    }
}

$hits = @(Get-ProductionHits)
$classified = New-Object System.Collections.Generic.List[object]
$unclassified = New-Object System.Collections.Generic.List[object]
$matchedAllowlist = New-Object System.Collections.Generic.HashSet[string]

foreach ($hit in $hits) {
    $match = $allowlist | Where-Object {
        $contextMatches = !$_.PSObject.Properties["contextContains"] -or
            $hit.Context.Contains($_.contextContains)
        $_.path -eq $hit.Path -and
            $_.kind -eq $hit.Kind -and
            $hit.Text.Contains($_.contains) -and
            $contextMatches
    } | Select-Object -First 1

    if ($match) {
        [void]$matchedAllowlist.Add("$($match.path)|$($match.kind)|$($match.contains)|$($match.contextContains)")
        $classified.Add([pscustomobject]@{
            Path = $hit.Path
            Line = $hit.Line
            Kind = $hit.Kind
            Classification = $match.classification
            Reason = $match.reason
        }) | Out-Null
    } else {
        $unclassified.Add($hit) | Out-Null
    }
}

$staleAllowlist = @(
    $allowlist | Where-Object {
        -not $matchedAllowlist.Contains("$($_.path)|$($_.kind)|$($_.contains)|$($_.contextContains)")
    }
)

if ($unclassified.Count -gt 0) {
    Write-Host "Unclassified production panic-surface hits:"
    $unclassified | Sort-Object Path, Line | Format-Table -AutoSize
}

if ($staleAllowlist.Count -gt 0) {
    Write-Host "Stale panic-surface allowlist entries:"
    $staleAllowlist | Sort-Object path, kind, contains | Format-Table -AutoSize
}

if ($unclassified.Count -gt 0 -or $staleAllowlist.Count -gt 0) {
    exit 1
}

Write-Host "Production panic-surface gate passed. Classified hits: $($classified.Count)."
