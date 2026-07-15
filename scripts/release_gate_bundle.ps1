param(
    [ValidateSet("all")]
    [string]$Profile = "all",
    [switch]$NoAllowDirty,
    [switch]$ExecuteNextWaveDryRun,
    [switch]$SkipLocalChecks,
    [switch]$SkipClippy,
    [switch]$LocalChecksOnly,
    [string]$TargetVersion,
    [string]$EvidenceDir = "..\docs\05_dev_plans\evidence"
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

. (Join-Path $PSScriptRoot "release_common.ps1")

function Write-Log {
    param(
        [string]$Path,
        [string]$Message
    )
    $line = "[{0}] {1}" -f (Get-Date -Format "HH:mm:ss"), $Message
    Write-Host $line
    Add-Content -Path $Path -Value $line -Encoding utf8
}

function Resolve-TargetVersion {
    param(
        [string]$Requested,
        [string]$WorkspaceRoot
    )

    return Resolve-ReleaseTargetVersion -Requested $Requested -WorkspaceRoot $WorkspaceRoot
}

function Resolve-PowerShellExecutable {
    $pwsh = Get-Command pwsh -ErrorAction SilentlyContinue
    if ($null -ne $pwsh) {
        return $pwsh.Source
    }

    $powershell = Get-Command powershell -ErrorAction SilentlyContinue
    if ($null -ne $powershell) {
        return $powershell.Source
    }

    throw "No PowerShell executable found (pwsh/powershell)."
}

function Get-LatestFile {
    param(
        [string]$Directory,
        [string]$Pattern,
        [datetime]$SinceUtc
    )

    $items = Get-ChildItem -Path $Directory -Filter $Pattern -File -ErrorAction SilentlyContinue |
        Where-Object { $_.LastWriteTimeUtc -ge $SinceUtc } |
        Sort-Object LastWriteTimeUtc -Descending

    if ($null -eq $items -or $items.Count -eq 0) {
        return $null
    }

    return $items[0]
}

function As-Array {
    param([object]$Value)

    if ($null -eq $Value) {
        return [object[]]@()
    }

    if ($Value -is [string]) {
        return [object[]]@($Value)
    }

    if ($Value -is [pscustomobject] -or $Value -is [hashtable]) {
        return [object[]]@($Value)
    }

    if ($Value -is [System.Array]) {
        return [object[]]@($Value)
    }

    if ($Value -is [System.Collections.IEnumerable]) {
        return [object[]]@($Value)
    }

    return [object[]]@($Value)
}

function New-GateCheckResult {
    param(
        [string]$Name,
        [string]$Command,
        [string]$WorkingDirectory,
        [int]$ExitCode,
        [bool]$Success,
        [string[]]$Output,
        [object]$Details = $null
    )

    return [ordered]@{
        name = $Name
        command = $Command
        working_directory = $WorkingDirectory
        exit_code = $ExitCode
        success = $Success
        output_tail = @($Output | Select-Object -Last 40 | ForEach-Object { [string]$_ })
        details = $Details
    }
}

function Get-StringSha256 {
    param([string[]]$Lines)

    $payload = [string]::Join("`n", @($Lines))
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($payload)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        return ([System.BitConverter]::ToString($sha256.ComputeHash($bytes))).Replace("-", "").ToLowerInvariant()
    } finally {
        $sha256.Dispose()
    }
}

function Get-TrackedRepositoryPaths {
    param([string]$RootWorkspace)

    $root = (Resolve-Path $RootWorkspace).Path
    $paths = New-Object System.Collections.Generic.List[string]
    $paths.Add($root)

    $previous = Get-Location
    try {
        Set-Location $root
        $submoduleRoots = @(& git submodule foreach --recursive --quiet 'git rev-parse --show-toplevel' 2>$null)
        $exitCode = $LASTEXITCODE
    } finally {
        Set-Location $previous
    }

    if ($exitCode -ne 0) {
        throw "Failed to enumerate recursive submodule repositories."
    }

    foreach ($path in $submoduleRoots) {
        $candidate = [string]$path
        if ([string]::IsNullOrWhiteSpace($candidate)) {
            continue
        }
        $paths.Add((Resolve-Path $candidate).Path)
    }

    return @($paths | Sort-Object -Unique)
}

function Get-TrackedTreeSnapshot {
    param([string]$RootWorkspace)

    $root = (Resolve-Path $RootWorkspace).Path
    $repositories = New-Object System.Collections.Generic.List[object]

    foreach ($repositoryPath in (Get-TrackedRepositoryPaths -RootWorkspace $root)) {
        $head = [string](& git -C $repositoryPath rev-parse HEAD 2>$null)
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to read HEAD for repository: $repositoryPath"
        }

        $status = @(& git -C $repositoryPath status --porcelain=v1 --untracked-files=no 2>$null | ForEach-Object { [string]$_ })
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to read tracked status for repository: $repositoryPath"
        }

        $diff = @(& git -C $repositoryPath diff --no-ext-diff --binary HEAD -- 2>$null | ForEach-Object { [string]$_ })
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to read tracked diff for repository: $repositoryPath"
        }

        $relativePath = [System.IO.Path]::GetRelativePath($root, $repositoryPath).Replace('\', '/')
        if ([string]::IsNullOrWhiteSpace($relativePath)) {
            $relativePath = "."
        }

        $repositories.Add([ordered]@{
            path = $relativePath
            head = $head.Trim()
            clean = ($status.Count -eq 0)
            status = $status
            tracked_diff_sha256 = Get-StringSha256 -Lines $diff
        })
    }

    return @($repositories.ToArray())
}

function Compare-TrackedTreeSnapshots {
    param(
        [object[]]$Before,
        [object[]]$After
    )

    $beforeByPath = @{}
    $afterByPath = @{}
    foreach ($repository in $Before) {
        $beforeByPath[[string]$repository.path] = $repository
    }
    foreach ($repository in $After) {
        $afterByPath[[string]$repository.path] = $repository
    }

    $paths = @($beforeByPath.Keys + $afterByPath.Keys | Sort-Object -Unique)
    $changes = New-Object System.Collections.Generic.List[object]
    foreach ($path in $paths) {
        if (-not $beforeByPath.ContainsKey($path) -or -not $afterByPath.ContainsKey($path)) {
            $changes.Add([ordered]@{
                path = $path
                reason = "repository set changed"
                before = $beforeByPath[$path]
                after = $afterByPath[$path]
            })
            continue
        }

        $beforeRepository = $beforeByPath[$path]
        $afterRepository = $afterByPath[$path]
        $beforeStatus = [string]::Join("`n", @($beforeRepository.status))
        $afterStatus = [string]::Join("`n", @($afterRepository.status))
        if (
            [string]$beforeRepository.head -ne [string]$afterRepository.head -or
            [string]$beforeRepository.tracked_diff_sha256 -ne [string]$afterRepository.tracked_diff_sha256 -or
            $beforeStatus -ne $afterStatus
        ) {
            $changes.Add([ordered]@{
                path = $path
                reason = "tracked state changed"
                before = $beforeRepository
                after = $afterRepository
            })
        }
    }

    return @($changes.ToArray())
}

function Invoke-GateCommand {
    param(
        [string]$Name,
        [string]$Executable,
        [string[]]$Arguments,
        [string]$WorkingDirectory,
        [string]$LogPath
    )

    $command = "$Executable $($Arguments -join ' ')"
    Write-Log -Path $LogPath -Message "running local gate: $Name"
    Write-Log -Path $LogPath -Message "command: $command"
    Write-Log -Path $LogPath -Message "working_directory: $WorkingDirectory"

    $previous = Get-Location
    $previousErrorActionPreference = $ErrorActionPreference
    try {
        Set-Location $WorkingDirectory
        $ErrorActionPreference = "Continue"
        $output = @(& $Executable @Arguments 2>&1 | ForEach-Object { [string]$_ })
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
        Set-Location $previous
    }

    if ($output.Count -gt 0) {
        Add-Content -Path $LogPath -Value $output -Encoding utf8
    }
    Write-Log -Path $LogPath -Message "$Name exit_code=$exitCode"

    return New-GateCheckResult `
        -Name $Name `
        -Command $command `
        -WorkingDirectory $WorkingDirectory `
        -ExitCode $exitCode `
        -Success ($exitCode -eq 0) `
        -Output $output
}

function Invoke-SubmoduleStatusGate {
    param(
        [string]$RootWorkspace,
        [string]$LogPath
    )

    $result = Invoke-GateCommand `
        -Name "root recursive submodule status" `
        -Executable "git" `
        -Arguments @("submodule", "status", "--recursive") `
        -WorkingDirectory $RootWorkspace `
        -LogPath $LogPath

    $badEntries = @(
        $result.output_tail |
            Where-Object { $_ -match '^[\-\+]' } |
            ForEach-Object { [string]$_ }
    )

    if ($badEntries.Count -gt 0) {
        $result.success = $false
        $result.details = [ordered]@{
            bad_entry_count = $badEntries.Count
            bad_entries = $badEntries
            note = "Submodule status entries beginning with '-' are uninitialized; '+' means checked out at a different commit than the root gitlink."
        }
        Write-Log -Path $LogPath -Message "root recursive submodule status drift detected: $($badEntries.Count) bad entr$(if ($badEntries.Count -eq 1) { 'y' } else { 'ies' })"
    }

    return $result
}

function Invoke-VersionDriftGate {
    param(
        [string]$RootWorkspace,
        [string]$RanvierWorkspace,
        [string]$ProfileKey,
        [string]$LogPath
    )

    $errors = New-Object System.Collections.Generic.List[string]
    $workspaceVersion = Get-WorkspaceReleaseVersion -WorkspaceRoot $RanvierWorkspace
    $registryPath = Join-Path $RootWorkspace "docs\05_dev_plans\CAPABILITY_REGISTRY.json"

    if (-not (Test-Path $registryPath)) {
        [void]$errors.Add("missing capability registry: $registryPath")
    } else {
        $registry = Get-Content -Path $registryPath -Raw | ConvertFrom-Json
        if ([string]$registry.version -ne $workspaceVersion) {
            [void]$errors.Add("registry.version=$($registry.version) does not match workspace.package.version=$workspaceVersion")
        }

        $ranvierModule = $registry.modules | Where-Object { $_.module -eq "ranvier" } | Select-Object -First 1
        if ($null -eq $ranvierModule) {
            [void]$errors.Add("missing ranvier module in capability registry")
        } else {
            if ([string]$ranvierModule.versioning.current -ne $workspaceVersion) {
                [void]$errors.Add("ranvier.versioning.current=$($ranvierModule.versioning.current) does not match workspace.package.version=$workspaceVersion")
            }

            $publishable = Resolve-ReleaseCrateSet -ProfileKey $ProfileKey -WorkspaceRoot $RanvierWorkspace
            $publishableSet = New-Object "System.Collections.Generic.HashSet[string]"
            foreach ($crate in $publishable) {
                [void]$publishableSet.Add([string]$crate)
            }

            foreach ($artifact in @($ranvierModule.versioning.artifacts)) {
                $artifactName = [string]$artifact.name
                if (-not $publishableSet.Contains($artifactName)) {
                    continue
                }
                if ([string]$artifact.version -ne $workspaceVersion) {
                    [void]$errors.Add("artifact $artifactName version=$($artifact.version) does not match workspace.package.version=$workspaceVersion")
                }
            }
        }
    }

    $success = ($errors.Count -eq 0)
    $details = [ordered]@{
        workspace_version = $workspaceVersion
        registry_path = $registryPath
        errors = @($errors.ToArray())
    }

    if ($success) {
        Write-Log -Path $LogPath -Message "version/capability drift gate passed (workspace_version=$workspaceVersion)"
    } else {
        foreach ($error in $errors) {
            Write-Log -Path $LogPath -Message "version/capability drift: $error"
        }
    }

    return New-GateCheckResult `
        -Name "version and capability drift" `
        -Command "compare ranvier/Cargo.toml workspace.package.version with docs/05_dev_plans/CAPABILITY_REGISTRY.json" `
        -WorkingDirectory $RootWorkspace `
        -ExitCode $(if ($success) { 0 } else { 1 }) `
        -Success $success `
        -Output @($errors.ToArray()) `
        -Details $details
}

function Invoke-LocalReleaseChecks {
    param(
        [string]$RootWorkspace,
        [string]$RanvierWorkspace,
        [string]$ProfileKey,
        [string]$LogPath,
        [bool]$SkipClippy,
        [bool]$RequireCleanTree
    )

    $checks = New-Object System.Collections.Generic.List[object]
    $treeBefore = Get-TrackedTreeSnapshot -RootWorkspace $RootWorkspace
    $dirtyBefore = @($treeBefore | Where-Object { -not [bool]$_.clean })
    $initialTreeSuccess = (-not $RequireCleanTree) -or ($dirtyBefore.Count -eq 0)
    $initialTreeOutput = @(
        $dirtyBefore | ForEach-Object {
            $repository = $_
            @($repository.status | ForEach-Object { "$($repository.path): $_" })
        }
    )
    $checks.Add((New-GateCheckResult `
        -Name "tracked tree initial state" `
        -Command "git status --porcelain=v1 --untracked-files=no (root and recursive submodules)" `
        -WorkingDirectory $RootWorkspace `
        -ExitCode $(if ($initialTreeSuccess) { 0 } else { 1 }) `
        -Success $initialTreeSuccess `
        -Output $initialTreeOutput `
        -Details @{
            require_clean = $RequireCleanTree
            dirty_repository_count = $dirtyBefore.Count
            repositories = $treeBefore
        }))

    if (-not $initialTreeSuccess) {
        Write-Log -Path $LogPath -Message "tracked tree initial-state gate failed: $($dirtyBefore.Count) dirty repositor$(if ($dirtyBefore.Count -eq 1) { 'y' } else { 'ies' })"
        return [ordered]@{
            enabled = $true
            total = $checks.Count
            passed = 0
            failed = 1
            failed_checks = @("tracked tree initial state")
            checks = $checks
            tree_guard = [ordered]@{
                require_clean = $RequireCleanTree
                before = $treeBefore
                after = $treeBefore
                changed = @()
            }
        }
    }

    $checks.Add((Invoke-SubmoduleStatusGate -RootWorkspace $RootWorkspace -LogPath $LogPath))
    $checks.Add((Invoke-VersionDriftGate -RootWorkspace $RootWorkspace -RanvierWorkspace $RanvierWorkspace -ProfileKey $ProfileKey -LogPath $LogPath))
    $checks.Add((Invoke-GateCommand -Name "ranvier cargo check workspace locked" -Executable "cargo" -Arguments @("check", "--workspace", "--locked") -WorkingDirectory $RanvierWorkspace -LogPath $LogPath))
    $checks.Add((Invoke-GateCommand -Name "ranvier cargo test workspace locked" -Executable "cargo" -Arguments @("test", "--workspace", "--locked") -WorkingDirectory $RanvierWorkspace -LogPath $LogPath))

    if ($SkipClippy) {
        Write-Log -Path $LogPath -Message "skipping publishable crate clippy gate by request"
        $checks.Add((New-GateCheckResult -Name "publishable crate clippy" -Command "cargo clippy -p <publishable> --all-targets -- -D warnings" -WorkingDirectory $RanvierWorkspace -ExitCode 0 -Success $true -Output @("skipped by -SkipClippy") -Details @{ skipped = $true }))
    } else {
        $publishableCrates = Resolve-ReleaseCrateSet -ProfileKey $ProfileKey -WorkspaceRoot $RanvierWorkspace
        $clippyArgs = @("clippy")
        foreach ($crate in $publishableCrates) {
            $clippyArgs += @("-p", [string]$crate)
        }
        $clippyArgs += @("--all-targets", "--locked", "--", "-D", "warnings")
        $checks.Add((Invoke-GateCommand -Name "publishable crate clippy" -Executable "cargo" -Arguments $clippyArgs -WorkingDirectory $RanvierWorkspace -LogPath $LogPath))
    }

    $cliRoot = Join-Path $RootWorkspace "cli"
    $studioServerRoot = Join-Path $RootWorkspace "studio-server"
    $checks.Add((Invoke-GateCommand -Name "cli cargo check locked" -Executable "cargo" -Arguments @("check", "--locked") -WorkingDirectory $cliRoot -LogPath $LogPath))
    $checks.Add((Invoke-GateCommand -Name "cli cargo test locked" -Executable "cargo" -Arguments @("test", "--locked") -WorkingDirectory $cliRoot -LogPath $LogPath))
    $checks.Add((Invoke-GateCommand -Name "studio-server cargo check locked" -Executable "cargo" -Arguments @("check", "--locked") -WorkingDirectory $studioServerRoot -LogPath $LogPath))
    $checks.Add((Invoke-GateCommand -Name "studio-server cargo test locked" -Executable "cargo" -Arguments @("test", "--locked") -WorkingDirectory $studioServerRoot -LogPath $LogPath))

    $treeAfter = Get-TrackedTreeSnapshot -RootWorkspace $RootWorkspace
    [object[]]$treeChanges = @(Compare-TrackedTreeSnapshots -Before $treeBefore -After $treeAfter)
    $treeUnchanged = ($treeChanges.Count -eq 0)
    $checks.Add((New-GateCheckResult `
        -Name "tracked tree unchanged" `
        -Command "compare tracked tree snapshot before and after local release gates" `
        -WorkingDirectory $RootWorkspace `
        -ExitCode $(if ($treeUnchanged) { 0 } else { 1 }) `
        -Success $treeUnchanged `
        -Output @($treeChanges | ForEach-Object { "$($_.path): $($_.reason)" }) `
        -Details @{
            changed_repository_count = $treeChanges.Count
            changes = @($treeChanges)
        }))

    if ($treeUnchanged) {
        Write-Log -Path $LogPath -Message "tracked tree unchanged after local release gates"
    } else {
        Write-Log -Path $LogPath -Message "tracked tree changed in $($treeChanges.Count) repositor$(if ($treeChanges.Count -eq 1) { 'y' } else { 'ies' })"
    }

    $failed = @($checks | Where-Object { -not [bool]$_.success })
    return [ordered]@{
        enabled = $true
        total = $checks.Count
        passed = ($checks.Count - $failed.Count)
        failed = $failed.Count
        failed_checks = @($failed | ForEach-Object { [string]$_.name })
        checks = $checks
        tree_guard = [ordered]@{
            require_clean = $RequireCleanTree
            before = $treeBefore
            after = $treeAfter
            changed = @($treeChanges)
        }
    }
}

$workspaceRoot = Get-ReleaseWorkspaceRoot -ScriptRoot $PSScriptRoot
$rootWorkspace = (Resolve-Path (Join-Path $workspaceRoot "..")).Path
$profileKey = $Profile.ToLowerInvariant()
$target = Resolve-TargetVersion -Requested $TargetVersion -WorkspaceRoot $workspaceRoot
$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$psExe = Resolve-PowerShellExecutable
$EvidenceDir = Resolve-ReleaseEvidenceDir -Requested $EvidenceDir -WorkspaceRoot $workspaceRoot

New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
$bundleLogPath = Join-Path $EvidenceDir "release_gate_bundle_${profileKey}_${timestamp}.log"
$bundleSummaryPath = Join-Path $EvidenceDir "release_gate_bundle_${profileKey}_${timestamp}.json"

Write-Log -Path $bundleLogPath -Message "release gate bundle started (profile=$profileKey, no_allow_dirty=$($NoAllowDirty.IsPresent), target=$target)"
Write-Log -Path $bundleLogPath -Message "workspace root: $workspaceRoot"
Write-Log -Path $bundleLogPath -Message "root workspace: $rootWorkspace"
Write-Log -Path $bundleLogPath -Message "powershell executable: $psExe"

$localChecks = [ordered]@{
    enabled = $false
    total = 0
    passed = 0
    failed = 0
    failed_checks = @()
    checks = @()
}

if ($SkipLocalChecks.IsPresent) {
    Write-Log -Path $bundleLogPath -Message "local release checks skipped by request"
} else {
    $localChecks = Invoke-LocalReleaseChecks -RootWorkspace $rootWorkspace -RanvierWorkspace $workspaceRoot -ProfileKey $profileKey -LogPath $bundleLogPath -SkipClippy:$SkipClippy.IsPresent -RequireCleanTree:$NoAllowDirty.IsPresent
    Write-Log -Path $bundleLogPath -Message "local release checks passed=$($localChecks.passed) failed=$($localChecks.failed)"

    if ([int]$localChecks.failed -gt 0) {
        $bundleSummary = [ordered]@{
            timestamp = $timestamp
            profile = $profileKey
            target_version = $target
            no_allow_dirty = $NoAllowDirty.IsPresent
            local_checks_only = $LocalChecksOnly.IsPresent
            local_checks = $localChecks
            preflight = $null
            wave_plan = $null
            registry_snapshot = $null
            next_publish_gate = $null
            next_publish_execute = $null
        }
        $bundleSummary | ConvertTo-Json -Depth 8 | Set-Content -Path $bundleSummaryPath -Encoding utf8
        Write-Log -Path $bundleLogPath -Message "bundle summary: $bundleSummaryPath"
        Write-Host "Evidence: $bundleLogPath"
        Write-Host "Summary:  $bundleSummaryPath"
        exit 1
    }
}

if ($LocalChecksOnly.IsPresent) {
    $bundleSummary = [ordered]@{
        timestamp = $timestamp
        profile = $profileKey
        target_version = $target
        no_allow_dirty = $NoAllowDirty.IsPresent
        local_checks_only = $true
        local_checks = $localChecks
        preflight = $null
        wave_plan = $null
        registry_snapshot = $null
        next_publish_gate = $null
        next_publish_execute = $null
    }
    $bundleSummary | ConvertTo-Json -Depth 8 | Set-Content -Path $bundleSummaryPath -Encoding utf8
    Write-Log -Path $bundleLogPath -Message "local checks only; skipping publish preflight and registry gates"
    Write-Log -Path $bundleLogPath -Message "bundle summary: $bundleSummaryPath"
    Write-Host "Evidence: $bundleLogPath"
    Write-Host "Summary:  $bundleSummaryPath"
    exit 0
}

$preflightScript = Join-Path $PSScriptRoot "publish_dry_run_preflight.ps1"
$waveScript = Join-Path $PSScriptRoot "plan_publish_waves.ps1"
$registryScript = Join-Path $PSScriptRoot "check_cratesio_versions.ps1"
$nextWaveScript = Join-Path $PSScriptRoot "plan_next_publish_wave.ps1"
$nextWaveExecuteScript = Join-Path $PSScriptRoot "execute_next_publish_wave.ps1"

$preflightStart = [datetime]::UtcNow
$preflightArgs = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $preflightScript, "-Profile", $profileKey)
if ($NoAllowDirty.IsPresent) {
    $preflightArgs += "-NoAllowDirty"
}

Write-Log -Path $bundleLogPath -Message "running preflight script..."
& $psExe @preflightArgs
$preflightExitCode = $LASTEXITCODE
Write-Log -Path $bundleLogPath -Message "preflight exit_code=$preflightExitCode"

$preflightSummaryFile = Get-LatestFile -Directory $EvidenceDir -Pattern "publish_dry_run_preflight_${profileKey}_*.json" -SinceUtc $preflightStart
if ($null -eq $preflightSummaryFile) {
    throw "Failed to locate preflight summary for profile=$profileKey"
}
Write-Log -Path $bundleLogPath -Message "preflight summary: $($preflightSummaryFile.FullName)"

$waveStart = [datetime]::UtcNow
$waveArgs = @(
    "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $waveScript,
    "-Profile", $profileKey,
    "-SummaryPath", $preflightSummaryFile.FullName
)

Write-Log -Path $bundleLogPath -Message "running wave planner..."
& $psExe @waveArgs
$waveExitCode = $LASTEXITCODE
Write-Log -Path $bundleLogPath -Message "wave planner exit_code=$waveExitCode"

$waveSummaryFile = Get-LatestFile -Directory $EvidenceDir -Pattern "publish_wave_plan_${profileKey}_*.json" -SinceUtc $waveStart
if ($null -eq $waveSummaryFile) {
    throw "Failed to locate wave summary for profile=$profileKey"
}
Write-Log -Path $bundleLogPath -Message "wave summary: $($waveSummaryFile.FullName)"

$registryStart = [datetime]::UtcNow
$registryArgs = @(
    "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $registryScript,
    "-Profile", $profileKey,
    "-TargetVersion", $target
)

Write-Log -Path $bundleLogPath -Message "running crates.io registry snapshot..."
& $psExe @registryArgs
$registryExitCode = $LASTEXITCODE
Write-Log -Path $bundleLogPath -Message "registry snapshot exit_code=$registryExitCode"

$registryPattern = "cratesio_version_snapshot_${profileKey}_${target}_*.json"
$registrySummaryFile = Get-LatestFile -Directory $EvidenceDir -Pattern $registryPattern -SinceUtc $registryStart
if ($null -eq $registrySummaryFile) {
    throw "Failed to locate registry snapshot summary for profile=$profileKey target=$target"
}
Write-Log -Path $bundleLogPath -Message "registry summary: $($registrySummaryFile.FullName)"

$nextWaveStart = [datetime]::UtcNow
$nextWaveArgs = @(
    "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $nextWaveScript,
    "-Profile", $profileKey,
    "-TargetVersion", $target,
    "-WaveSummaryPath", $waveSummaryFile.FullName,
    "-RegistrySummaryPath", $registrySummaryFile.FullName
)

Write-Log -Path $bundleLogPath -Message "running next-wave gate planner..."
& $psExe @nextWaveArgs
$nextWaveExitCode = $LASTEXITCODE
Write-Log -Path $bundleLogPath -Message "next-wave gate planner exit_code=$nextWaveExitCode"

$nextWavePattern = "publish_next_wave_gate_${profileKey}_${target}_*.json"
$nextWaveSummaryFile = Get-LatestFile -Directory $EvidenceDir -Pattern $nextWavePattern -SinceUtc $nextWaveStart
if ($null -eq $nextWaveSummaryFile) {
    throw "Failed to locate next-wave gate summary for profile=$profileKey target=$target"
}
Write-Log -Path $bundleLogPath -Message "next-wave gate summary: $($nextWaveSummaryFile.FullName)"

$nextWaveExecuteExitCode = 0
$nextWaveExecuteSummaryFile = $null
if ($ExecuteNextWaveDryRun.IsPresent) {
    $nextWaveExecuteStart = [datetime]::UtcNow
    $nextWaveExecuteArgs = @(
        "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $nextWaveExecuteScript,
        "-Profile", $profileKey,
        "-TargetVersion", $target,
        "-Mode", "dry-run",
        "-NextWaveSummaryPath", $nextWaveSummaryFile.FullName
    )
    if (-not $NoAllowDirty.IsPresent) {
        $nextWaveExecuteArgs += "-AllowDirty"
    }

    Write-Log -Path $bundleLogPath -Message "running next-wave execution (dry-run)..."
    & $psExe @nextWaveExecuteArgs
    $nextWaveExecuteExitCode = $LASTEXITCODE
    Write-Log -Path $bundleLogPath -Message "next-wave execution exit_code=$nextWaveExecuteExitCode"

    $nextWaveExecutePattern = "publish_next_wave_execute_${profileKey}_${target}_*.json"
    $nextWaveExecuteSummaryFile = Get-LatestFile -Directory $EvidenceDir -Pattern $nextWaveExecutePattern -SinceUtc $nextWaveExecuteStart
    if ($null -eq $nextWaveExecuteSummaryFile) {
        throw "Failed to locate next-wave execute summary for profile=$profileKey target=$target"
    }
    Write-Log -Path $bundleLogPath -Message "next-wave execute summary: $($nextWaveExecuteSummaryFile.FullName)"
}

$preflightSummary = Get-Content -Path $preflightSummaryFile.FullName -Raw | ConvertFrom-Json
$waveSummary = Get-Content -Path $waveSummaryFile.FullName -Raw | ConvertFrom-Json
$registrySummary = Get-Content -Path $registrySummaryFile.FullName -Raw | ConvertFrom-Json
$nextWaveSummary = Get-Content -Path $nextWaveSummaryFile.FullName -Raw | ConvertFrom-Json
$nextWaveExecuteSummary = $null
if ($null -ne $nextWaveExecuteSummaryFile) {
    $nextWaveExecuteSummary = Get-Content -Path $nextWaveExecuteSummaryFile.FullName -Raw | ConvertFrom-Json
}

$wave1Crates = @(
    $waveSummary.waves |
        Select-Object -First 1 |
        ForEach-Object { $_.crates } |
        ForEach-Object { $_ } |
        ForEach-Object { [string]$_ } |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
)
$publishCommandsWave1 = @($wave1Crates | ForEach-Object { "cargo publish -p $_" })

$bundleSummary = [ordered]@{
    timestamp = $timestamp
    profile = $profileKey
    target_version = $target
    no_allow_dirty = $NoAllowDirty.IsPresent
    local_checks_only = $LocalChecksOnly.IsPresent
    local_checks = $localChecks
    preflight = [ordered]@{
        exit_code = $preflightExitCode
        summary_path = $preflightSummaryFile.FullName
        passed = [int]$preflightSummary.passed
        failed = [int]$preflightSummary.failed
        failed_crates = @(As-Array $preflightSummary.failed_crates | ForEach-Object { [string]$_ })
    }
    wave_plan = [ordered]@{
        exit_code = $waveExitCode
        summary_path = $waveSummaryFile.FullName
        wave_count = [int]$waveSummary.wave_count
        blocked_count = [int]$waveSummary.blocked_count
        wave1_crates = $wave1Crates
        publish_commands_wave1 = $publishCommandsWave1
    }
    registry_snapshot = [ordered]@{
        exit_code = $registryExitCode
        summary_path = $registrySummaryFile.FullName
        found = [int]$registrySummary.found
        not_found = [int]$registrySummary.not_found
        target_present_count = [int]$registrySummary.target_present_count
        target_missing_count = [int]$registrySummary.target_missing_count
        target_present_crates = @(As-Array $registrySummary.target_present_crates | ForEach-Object { [string]$_ })
        target_missing_crates = @(As-Array $registrySummary.target_missing_crates | ForEach-Object { [string]$_ })
        not_found_crates = @(As-Array $registrySummary.not_found_crates | ForEach-Object { [string]$_ })
    }
    next_publish_gate = [ordered]@{
        exit_code = $nextWaveExitCode
        summary_path = $nextWaveSummaryFile.FullName
        all_waves_complete = [bool]$nextWaveSummary.all_waves_complete
        next_publish_wave = $nextWaveSummary.next_publish_wave
        next_publish_crates = @(As-Array $nextWaveSummary.next_publish_crates | ForEach-Object { [string]$_ })
        next_publish_commands = @(As-Array $nextWaveSummary.next_publish_commands | ForEach-Object { [string]$_ })
    }
    next_publish_execute = [ordered]@{
        enabled = $ExecuteNextWaveDryRun.IsPresent
        mode = if ($ExecuteNextWaveDryRun.IsPresent) { "dry-run" } else { $null }
        exit_code = if ($ExecuteNextWaveDryRun.IsPresent) { $nextWaveExecuteExitCode } else { $null }
        summary_path = if ($null -ne $nextWaveExecuteSummaryFile) { $nextWaveExecuteSummaryFile.FullName } else { $null }
        skipped = if ($null -ne $nextWaveExecuteSummary) { [bool]$nextWaveExecuteSummary.skipped } else { $null }
        next_publish_wave = if ($null -ne $nextWaveExecuteSummary) { $nextWaveExecuteSummary.next_publish_wave } else { $null }
        next_publish_crates = if ($null -ne $nextWaveExecuteSummary) { @(As-Array $nextWaveExecuteSummary.next_publish_crates | ForEach-Object { [string]$_ }) } else { @() }
    }
}

$bundleSummary | ConvertTo-Json -Depth 8 | Set-Content -Path $bundleSummaryPath -Encoding utf8

Write-Log -Path $bundleLogPath -Message "wave1 crates: $($wave1Crates -join ', ')"
Write-Log -Path $bundleLogPath -Message "registry target_present_count=$($bundleSummary.registry_snapshot.target_present_count)"
Write-Log -Path $bundleLogPath -Message "next publish wave=$($bundleSummary.next_publish_gate.next_publish_wave)"
if ($ExecuteNextWaveDryRun.IsPresent) {
    Write-Log -Path $bundleLogPath -Message "next-wave execution enabled: exit_code=$nextWaveExecuteExitCode"
}
Write-Log -Path $bundleLogPath -Message "bundle summary: $bundleSummaryPath"

Write-Host "Evidence: $bundleLogPath"
Write-Host "Summary:  $bundleSummaryPath"

if ($preflightExitCode -ne 0) {
    exit $preflightExitCode
}

if ($ExecuteNextWaveDryRun.IsPresent -and $nextWaveExecuteExitCode -ne 0) {
    exit $nextWaveExecuteExitCode
}
