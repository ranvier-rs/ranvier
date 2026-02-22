param(
    [ValidateSet("m119", "m131", "all")]
    [string]$Train = "m119",
    [string]$TargetVersion,
    [switch]$Apply,
    [switch]$NoWorkspacePackageBump,
    [switch]$NoIncludeWorkspaceVersionUsers,
    [string]$EvidenceDir = "..\docs\05_dev_plans\evidence"
)

$ErrorActionPreference = "Stop"

function Resolve-TrainCrates {
    param([string]$Key)

    $m119 = @(
        "ranvier-core",
        "ranvier-runtime",
        "ranvier-http",
        "ranvier-std",
        "ranvier-macros",
        "ranvier"
    )
    $m131 = @(
        "ranvier-auth",
        "ranvier-guard",
        "ranvier-inspector",
        "ranvier-observe",
        "ranvier-runtime",
        "ranvier-http",
        "ranvier-openapi",
        "ranvier"
    )

    switch ($Key) {
        "m119" { return $m119 }
        "m131" { return $m131 }
        "all" {
            $ordered = New-Object System.Collections.Generic.List[string]
            foreach ($name in ($m119 + $m131)) {
                if (-not $ordered.Contains($name)) {
                    [void]$ordered.Add($name)
                }
            }
            return @($ordered)
        }
        default { throw "Unknown train: $Key" }
    }
}

function Infer-TargetVersion {
    param(
        [string]$TrainKey,
        [string]$Requested
    )

    if (-not [string]::IsNullOrWhiteSpace($Requested)) {
        return $Requested.Trim()
    }

    switch ($TrainKey) {
        "m119" { return "0.2.0" }
        "m131" { return "0.7.0" }
        "all" { return "0.7.0" }
        default { throw "TargetVersion is required for train=$TrainKey" }
    }
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

$workspaceRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$trainKey = $Train.ToLowerInvariant()
$targetVersion = Infer-TargetVersion -TrainKey $trainKey -Requested $TargetVersion
$bumpWorkspacePackage = -not $NoWorkspacePackageBump
$includeWorkspaceVersionUsers = -not $NoIncludeWorkspaceVersionUsers
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"

New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
$evidencePath = Join-Path $EvidenceDir "release_version_bump_plan_${trainKey}_${targetVersion}_${timestamp}.log"
$summaryPath = Join-Path $EvidenceDir "release_version_bump_plan_${trainKey}_${targetVersion}_${timestamp}.json"

Write-Log "Release version bump planning started (train=$trainKey, target=$targetVersion, apply=$($Apply.IsPresent), workspace_package_bump=$bumpWorkspacePackage, include_workspace_users=$includeWorkspaceVersionUsers)" -Path $evidencePath
Write-Log "Workspace root: $workspaceRoot" -Path $evidencePath

$trainCrates = @((Resolve-TrainCrates -Key $trainKey) | Sort-Object -Unique)
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

$summary = [ordered]@{
    timestamp = $timestamp
    train = $trainKey
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
