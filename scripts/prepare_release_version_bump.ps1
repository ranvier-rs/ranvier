param(
    [ValidateSet("all")]
    [string]$Train = "all",
    [string]$TargetVersion,
    [switch]$Apply,
    [switch]$NoWorkspacePackageBump,
    [switch]$NoIncludeWorkspaceVersionUsers,
    [string]$EvidenceDir = "..\docs\05_dev_plans\evidence"
)

$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "release_common.ps1")

function Resolve-TrainCrates {
    param(
        [string]$Key,
        [string]$WorkspaceRoot
    )

    return Resolve-ReleaseCrateSet -ProfileKey $Key -WorkspaceRoot $WorkspaceRoot
}

function Infer-TargetVersion {
    param(
        [string]$Requested,
        [string]$WorkspaceRoot
    )

    return Resolve-ReleaseTargetVersion -Requested $Requested -WorkspaceRoot $WorkspaceRoot
}

function Write-Log {
    param(
        [string]$Message,
        [string]$Path
    )
    $line = "[{0}] {1}" -f (Get-Date -Format "HH:mm:ss"), $Message
    Write-Host $line
    Add-Content -Path $Path -Value $line -Encoding utf8
}

function Write-FileUtf8NoBom {
    param(
        [string]$Path,
        [string[]]$Lines
    )

    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    $content = [string]::Join([Environment]::NewLine, $Lines)
    [System.IO.File]::WriteAllText($Path, $content, $utf8NoBom)
}

$workspaceRoot = Get-ReleaseWorkspaceRoot -ScriptRoot $PSScriptRoot
$trainKey = $Train.ToLowerInvariant()
$targetVersion = Infer-TargetVersion -Requested $TargetVersion -WorkspaceRoot $workspaceRoot
$bumpWorkspacePackage = -not $NoWorkspacePackageBump
$includeWorkspaceVersionUsers = -not $NoIncludeWorkspaceVersionUsers
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$EvidenceDir = Resolve-ReleaseEvidenceDir -Requested $EvidenceDir -WorkspaceRoot $workspaceRoot
# Read before any edit: the version the workspace is moving away from.
$previousVersion = Get-WorkspaceReleaseVersion -WorkspaceRoot $workspaceRoot

New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
$evidencePath = Join-Path $EvidenceDir "release_version_bump_plan_${trainKey}_${targetVersion}_${timestamp}.log"
$summaryPath = Join-Path $EvidenceDir "release_version_bump_plan_${trainKey}_${targetVersion}_${timestamp}.json"

Write-Log "Release version bump planning started (train=$trainKey, target=$targetVersion, apply=$($Apply.IsPresent), workspace_package_bump=$bumpWorkspacePackage, include_workspace_users=$includeWorkspaceVersionUsers)" -Path $evidencePath
Write-Log "Workspace root: $workspaceRoot" -Path $evidencePath

$trainCrates = @((Resolve-TrainCrates -Key $trainKey -WorkspaceRoot $workspaceRoot) | Sort-Object -Unique)
$trainSet = New-Object "System.Collections.Generic.HashSet[string]"
foreach ($crate in $trainCrates) {
    [void]$trainSet.Add($crate)
}
Write-Log "Train crates: $($trainCrates -join ', ')" -Path $evidencePath

$manifestPath = Join-Path $workspaceRoot "Cargo.toml"
$metadataRaw = & cargo metadata --format-version 1 --no-deps --offline --manifest-path $manifestPath 2>&1
if ($LASTEXITCODE -ne 0) {
    throw "Failed to resolve workspace metadata."
}
$metadata = $metadataRaw | ConvertFrom-Json

$workspaceMemberIds = New-Object "System.Collections.Generic.HashSet[string]"
foreach ($memberId in $metadata.workspace_members) {
    [void]$workspaceMemberIds.Add($memberId)
}

$memberPackages = @($metadata.packages | Where-Object { $workspaceMemberIds.Contains($_.id) })
$packageByManifest = @{}
$packageByName = @{}
foreach ($pkg in $memberPackages) {
    $manifest = [string]$pkg.manifest_path
    $manifest = $manifest.Replace("/", "\")
    $packageByManifest[$manifest] = $pkg
    $packageByName[$pkg.name] = $pkg
}

foreach ($crate in $trainCrates) {
    if (-not $packageByName.ContainsKey($crate)) {
        Write-Log "WARN: train crate not found in workspace metadata: $crate" -Path $evidencePath
    }
}

$workspaceVersionUsersDetected = New-Object System.Collections.Generic.List[string]
foreach ($pkg in $memberPackages) {
    $pkgManifestPath = ([string]$pkg.manifest_path).Replace("/", "\")
    $manifestLines = Get-Content -Path $pkgManifestPath
    if ($manifestLines -match '^\s*version\.workspace\s*=\s*true\s*$') {
        [void]$workspaceVersionUsersDetected.Add([string]$pkg.name)
    }
}

$effectiveCrates = New-Object System.Collections.Generic.List[string]
foreach ($crate in $trainCrates) {
    [void]$effectiveCrates.Add($crate)
}

if ($bumpWorkspacePackage -and $includeWorkspaceVersionUsers) {
    foreach ($pkgName in @($workspaceVersionUsersDetected | Sort-Object -Unique)) {
        if (-not $effectiveCrates.Contains($pkgName)) {
            [void]$effectiveCrates.Add($pkgName)
        }
    }
}

$effectiveCrates = @($effectiveCrates | Sort-Object -Unique)
$crateSet = New-Object "System.Collections.Generic.HashSet[string]"
foreach ($crate in $effectiveCrates) {
    [void]$crateSet.Add($crate)
}

if ($effectiveCrates.Count -ne $trainCrates.Count) {
    $autoIncluded = @($effectiveCrates | Where-Object { $trainCrates -notcontains $_ })
    Write-Log "Auto-included workspace-version crates: $($autoIncluded -join ', ')" -Path $evidencePath
}
Write-Log "Effective crates for version edits: $($effectiveCrates -join ', ')" -Path $evidencePath

$manifestPaths = New-Object System.Collections.Generic.List[string]
[void]$manifestPaths.Add((Resolve-Path $manifestPath).Path)
foreach ($pkg in $memberPackages | Sort-Object name) {
    [void]$manifestPaths.Add(([string]$pkg.manifest_path).Replace("/", "\"))
}

$workspaceManifestAbs = (Resolve-Path $manifestPath).Path
$changedFiles = New-Object System.Collections.Generic.List[object]
# Post-edit text of every file this run changes (or would change without
# -Apply), keyed by full path, so the mention report describes the bumped tree.
$pendingContent = @{}
$workspaceVersionUsers = New-Object System.Collections.Generic.List[string]
$workspaceVersionUsersInTrain = New-Object System.Collections.Generic.List[string]
$workspaceVersionUsersInEffective = New-Object System.Collections.Generic.List[string]

foreach ($filePath in $manifestPaths) {
    $absPath = (Resolve-Path $filePath).Path
    $pkg = $null
    $pkgName = $null
    if ($packageByManifest.ContainsKey($absPath)) {
        $pkg = $packageByManifest[$absPath]
        $pkgName = [string]$pkg.name
    }

    $lines = Get-Content -Path $absPath
    $newLines = New-Object System.Collections.Generic.List[string]
    $edits = New-Object System.Collections.Generic.List[object]
    $section = ""
    $isPackageVersionWorkspace = $false

    foreach ($line in $lines) {
        $updatedLine = $line

        if ($line -match '^\s*\[([^\]]+)\]\s*$') {
            $section = $matches[1].Trim()
        }

        if (($section -eq "package") -and ($line -match '^\s*version\.workspace\s*=\s*true\s*$')) {
            $isPackageVersionWorkspace = $true
        }

        if (($absPath -eq $workspaceManifestAbs) -and $bumpWorkspacePackage -and ($section -eq "workspace.package") -and ($line -match '^\s*version\s*=\s*"([^"]+)"\s*$')) {
            $old = $matches[1]
            if ($old -ne $targetVersion) {
                $updatedLine = $line -replace '(^\s*version\s*=\s*")[^"]+(".*$)', "`${1}$targetVersion`${2}"
                $edits.Add([ordered]@{
                    kind = "workspace.package.version"
                    before = $old
                    after = $targetVersion
                })
            }
        }

        if (($section -eq "package") -and ($pkgName -ne $null) -and $crateSet.Contains($pkgName) -and ($line -match '^\s*version\s*=\s*"([^"]+)"\s*$')) {
            $old = $matches[1]
            if ($old -ne $targetVersion) {
                $updatedLine = $line -replace '(^\s*version\s*=\s*")[^"]+(".*$)', "`${1}$targetVersion`${2}"
                $edits.Add([ordered]@{
                    kind = "package.version"
                    package = $pkgName
                    before = $old
                    after = $targetVersion
                })
            }
        }

        foreach ($crate in $effectiveCrates) {
            $escapedCrate = [regex]::Escape($crate)
            if ($updatedLine -match "^\s*$escapedCrate\s*=\s*\{.*\bversion\s*=\s*`"([^`"]+)`".*\}\s*$") {
                $old = $matches[1]
                if ($old -ne $targetVersion) {
                    $updatedLine = [regex]::Replace(
                        $updatedLine,
                        '(\bversion\s*=\s*")[^"]+(")',
                        {
                            param($match)
                            return "$($match.Groups[1].Value)$targetVersion$($match.Groups[2].Value)"
                        },
                        1
                    )
                    $edits.Add([ordered]@{
                        kind = "dependency.version"
                        dependency = $crate
                        before = $old
                        after = $targetVersion
                    })
                }
            }
        }

        [void]$newLines.Add($updatedLine)
    }

    if ($pkgName -ne $null -and $isPackageVersionWorkspace) {
        [void]$workspaceVersionUsers.Add($pkgName)
        if ($trainSet.Contains($pkgName)) {
            [void]$workspaceVersionUsersInTrain.Add($pkgName)
        }
        if ($crateSet.Contains($pkgName)) {
            [void]$workspaceVersionUsersInEffective.Add($pkgName)
        }
    }

    if ($edits.Count -gt 0) {
        $pendingContent[[System.IO.Path]::GetFullPath($absPath)] = [string]::Join("`n", @($newLines))
        if ($Apply.IsPresent) {
            Write-FileUtf8NoBom -Path $absPath -Lines @($newLines)
        }

        $relative = Resolve-Path -Relative $absPath
        $changedFiles.Add([ordered]@{
            path = $relative
            edit_count = $edits.Count
            edits = $edits
        })
    }
}

# Current-version fields outside Cargo manifests that gates compare with the
# ranvier-core version (api_surface_inventory.mjs) or that describe the
# workspace release. Only the top-level key (two-space indent) is edited, as
# text, so the file keeps its formatting and line endings.
$versionFieldSpecs = @(
    @{ path = ".ranvier-api-policy.json"; key = "product_version" },
    @{ path = ".ranvier-observe-api-policy.json"; key = "product_version" },
    @{ path = ".ranvier-examples-manifest.json"; key = "version" },
    @{ path = "examples/catalog.json"; key = "workspace_version" }
)
$versionFields = New-Object System.Collections.Generic.List[object]
$warnings = New-Object System.Collections.Generic.List[string]
$utf8NoBomEncoding = New-Object System.Text.UTF8Encoding($false)
if ($bumpWorkspacePackage -and ($previousVersion -ne $targetVersion)) {
    foreach ($spec in $versionFieldSpecs) {
        $fieldPath = Join-Path $workspaceRoot $spec.path
        $record = [ordered]@{ path = $spec.path; key = $spec.key; before = $null; after = $null; status = "" }
        if (-not (Test-Path -LiteralPath $fieldPath)) {
            $record.status = "missing-file"
        } else {
            $text = [System.IO.File]::ReadAllText($fieldPath)
            $pattern = '(?m)^(  "' + [regex]::Escape($spec.key) + '"\s*:\s*")([^"]*)(")'
            $found = [regex]::Matches($text, $pattern)
            if ($found.Count -ne 1) {
                $record.status = "key-count-$($found.Count)"
            } elseif ($found[0].Groups[2].Value -ne $previousVersion) {
                $record.before = $found[0].Groups[2].Value
                $record.status = "unexpected-value"
            } else {
                $record.before = $previousVersion
                $record.after = $targetVersion
                $record.status = "updated"
                $updated = [regex]::Replace($text, $pattern, {
                    param($match)
                    return "$($match.Groups[1].Value)$targetVersion$($match.Groups[3].Value)"
                }, 1)
                $pendingContent[[System.IO.Path]::GetFullPath($fieldPath)] = $updated
                if ($Apply.IsPresent) {
                    [System.IO.File]::WriteAllText($fieldPath, $updated, $utf8NoBomEncoding)
                }
            }
        }
        if ($record.status -ne "updated") {
            [void]$warnings.Add("version field $($spec.path) $($spec.key): $($record.status)")
            Write-Log "WARN: version field $($spec.path) $($spec.key) not changed ($($record.status))" -Path $evidencePath
        } else {
            Write-Log "Version field $($spec.path) $($spec.key): $previousVersion -> $targetVersion" -Path $evidencePath
        }
        [void]$versionFields.Add($record)
    }
}

# Report the remaining mentions of the previous version so that a release
# does not ship stale numbers. Rules live in release_version_mentions.json;
# nothing here fails the run because this is a planning tool.
$mentionReport = [ordered]@{ searched = $false; pattern = $null; by_category = [ordered]@{}; unclassified = @(); next_steps = @() }
$rulesPath = Join-Path $PSScriptRoot "release_version_mentions.json"
if (($previousVersion -ne $targetVersion) -and (Test-Path -LiteralPath $rulesPath)) {
    $rules = Get-Content -LiteralPath $rulesPath -Raw | ConvertFrom-Json
    $previousParts = $previousVersion.Split("-")[0].Split(".")
    $targetParts = $targetVersion.Split("-")[0].Split(".")
    $sameLine = ($previousParts[0] -eq $targetParts[0]) -and ($previousParts[1] -eq $targetParts[1])
    if ($sameLine) {
        # A leading "v" (tags such as v1.2.3) still counts as a mention.
        $mentionPattern = '(?<![\d.])' + [regex]::Escape($previousVersion) + '(?!\.?\d)'
    } else {
        $linePrefix = "$($previousParts[0]).$($previousParts[1])"
        $mentionPattern = '(?<![\d.])' + [regex]::Escape($linePrefix) + '(?!\d)'
    }
    $mentionReport.searched = $true
    $mentionReport.pattern = $mentionPattern

    $trackedRaw = & git -C $workspaceRoot ls-files 2>$null
    if ($LASTEXITCODE -ne 0) {
        [void]$warnings.Add("git ls-files failed; mention report skipped")
        $trackedRaw = @()
    }
    $byCategory = @{}
    $unclassified = New-Object System.Collections.Generic.List[string]
    foreach ($relative in $trackedRaw) {
        $relative = [string]$relative
        $abs = Join-Path $workspaceRoot $relative
        if (-not (Test-Path -LiteralPath $abs -PathType Leaf)) { continue }
        $full = [System.IO.Path]::GetFullPath($abs)
        $hits = New-Object System.Collections.Generic.List[object]
        if ($pendingContent.ContainsKey($full)) {
            $number = 0
            foreach ($text in ($pendingContent[$full] -split "\r?\n")) {
                $number++
                if ($text -match $mentionPattern) { [void]$hits.Add([pscustomobject]@{ LineNumber = $number; Line = $text }) }
            }
        } else {
            foreach ($hit in @(Select-String -LiteralPath $abs -Pattern $mentionPattern -ErrorAction SilentlyContinue)) {
                [void]$hits.Add([pscustomobject]@{ LineNumber = $hit.LineNumber; Line = $hit.Line })
            }
        }
        if ($hits.Count -eq 0) { continue }
        $category = $null
        foreach ($rule in $rules.rules) {
            foreach ($glob in $rule.paths) {
                if ($relative -like $glob) { $category = [string]$rule.category; break }
            }
            if ($category) { break }
        }
        foreach ($hit in $hits) {
            $line = "${relative}:$($hit.LineNumber): $($hit.Line.Trim())"
            if ($line.Length -gt 200) { $line = $line.Substring(0, 200) }
            if ($category) {
                if (-not $byCategory.ContainsKey($category)) { $byCategory[$category] = New-Object System.Collections.Generic.List[string] }
                [void]$byCategory[$category].Add($line)
            } else {
                [void]$unclassified.Add($line)
            }
        }
    }
    foreach ($rule in $rules.rules) {
        $category = [string]$rule.category
        if ($mentionReport.by_category.Contains($category)) { continue }
        $lines = @()
        if ($byCategory.ContainsKey($category)) { $lines = @($byCategory[$category]) }
        $mentionReport.by_category[$category] = $lines
    }
    $mentionReport.unclassified = @($unclassified)
    $mentionReport.next_steps = @($rules.next_steps)

    foreach ($category in $mentionReport.by_category.Keys) {
        Write-Log "Previous-version mentions [$category]: $(@($mentionReport.by_category[$category]).Count) lines" -Path $evidencePath
    }
    if ($unclassified.Count -gt 0) {
        [void]$warnings.Add("unclassified previous-version mentions: $($unclassified.Count)")
        Write-Log "WARN: unclassified previous-version mentions: $($unclassified.Count) (add a rule or update the line)" -Path $evidencePath
        foreach ($line in $unclassified) { Write-Log "  ? $line" -Path $evidencePath }
    }
    foreach ($step in $mentionReport.next_steps) { Write-Log "Next: $step" -Path $evidencePath }
}

$summary = [ordered]@{
    timestamp = $timestamp
    train = $trainKey
    previous_version = $previousVersion
    target_version = $targetVersion
    apply = $Apply.IsPresent
    workspace_package_bump = $bumpWorkspacePackage
    workspace_root = "$workspaceRoot"
    train_crates = @($trainCrates)
    effective_crates = @($effectiveCrates)
    workspace_version_users = @($workspaceVersionUsers | Sort-Object -Unique)
    workspace_version_users_in_train = @($workspaceVersionUsersInTrain | Sort-Object -Unique)
    workspace_version_users_in_effective = @($workspaceVersionUsersInEffective | Sort-Object -Unique)
    changed_file_count = $changedFiles.Count
    changed_files = $changedFiles
    version_fields = $versionFields
    previous_version_mentions = $mentionReport
    warnings = @($warnings)
}

$summary | ConvertTo-Json -Depth 8 | Set-Content -Path $summaryPath -Encoding utf8
Write-Log "Summary JSON: $summaryPath" -Path $evidencePath

if ($changedFiles.Count -eq 0) {
    Write-Log "No version changes required for train=$trainKey target=$targetVersion" -Path $evidencePath
} else {
    Write-Log "Planned version edits: $($changedFiles.Count) files" -Path $evidencePath
    foreach ($changed in $changedFiles) {
        Write-Log "  - $($changed.path) ($($changed.edit_count) edits)" -Path $evidencePath
    }
}

Write-Host "Evidence: $evidencePath"
Write-Host "Summary:  $summaryPath"
