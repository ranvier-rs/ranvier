function Get-ReleaseWorkspaceRoot {
    param([string]$ScriptRoot)

    return (Resolve-Path (Join-Path $ScriptRoot "..")).Path
}

function Resolve-ReleaseEvidenceDir {
    param(
        [string]$Requested,
        [string]$WorkspaceRoot
    )

    if ([string]::IsNullOrWhiteSpace($Requested)) {
        return (Join-Path (Resolve-Path (Join-Path $WorkspaceRoot "..")).Path "docs\05_dev_plans\evidence")
    }

    if ([System.IO.Path]::IsPathRooted($Requested)) {
        return $Requested
    }

    return (Join-Path $WorkspaceRoot $Requested)
}

function Get-WorkspaceReleaseVersion {
    param([string]$WorkspaceRoot)

    $manifestPath = Join-Path $WorkspaceRoot "Cargo.toml"
    $section = ""
    foreach ($line in Get-Content -Path $manifestPath) {
        if ($line -match '^\s*\[([^\]]+)\]\s*$') {
            $section = $matches[1].Trim()
            continue
        }

        if (($section -eq "workspace.package") -and ($line -match '^\s*version\s*=\s*"([^"]+)"\s*$')) {
            return $matches[1]
        }
    }

    throw "Failed to resolve workspace.package.version from $manifestPath"
}

function Resolve-ReleaseTargetVersion {
    param(
        [string]$Requested,
        [string]$WorkspaceRoot
    )

    if (-not [string]::IsNullOrWhiteSpace($Requested)) {
        return $Requested.Trim()
    }

    return Get-WorkspaceReleaseVersion -WorkspaceRoot $WorkspaceRoot
}

function Get-PublishableWorkspaceCrates {
    param([string]$WorkspaceRoot)

    $manifestPath = Join-Path $WorkspaceRoot "Cargo.toml"
    $metadataRaw = & cargo metadata --format-version 1 --no-deps --offline --manifest-path $manifestPath 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to resolve cargo metadata for publishable crates."
    }

    $metadata = $metadataRaw | ConvertFrom-Json
    $workspaceMemberIds = New-Object "System.Collections.Generic.HashSet[string]"
    foreach ($memberId in $metadata.workspace_members) {
        [void]$workspaceMemberIds.Add([string]$memberId)
    }

    $crates = New-Object System.Collections.Generic.List[string]
    foreach ($pkg in $metadata.packages) {
        if (-not $workspaceMemberIds.Contains([string]$pkg.id)) {
            continue
        }
        if (-not ([string]$pkg.name).StartsWith("ranvier")) {
            continue
        }
        if ($pkg.publish -is [bool] -and -not [bool]$pkg.publish) {
            continue
        }
        if ($pkg.publish -is [System.Array] -and $pkg.publish.Count -eq 0) {
            continue
        }

        [void]$crates.Add([string]$pkg.name)
    }

    return @($crates | Sort-Object -Unique)
}

function Resolve-ReleaseCrateSet {
    param(
        [string]$ProfileKey,
        [string]$WorkspaceRoot
    )

    $all = Get-PublishableWorkspaceCrates -WorkspaceRoot $WorkspaceRoot
    switch ($ProfileKey.ToLowerInvariant()) {
        "all" { return $all }
        "m119" { return $all }
        "m131" { return $all }
        default { throw "Unknown release profile: $ProfileKey" }
    }
}
