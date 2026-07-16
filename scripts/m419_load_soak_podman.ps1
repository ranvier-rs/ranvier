param(
    [ValidateSet("Full", "Quick")]
    [string]$Mode = "Full",
    [string]$Image = "localhost/ranvier-m419-load-soak:rust-1.95-node-24",
    [string]$OutputRoot = ""
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

function Invoke-Podman {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments,
        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    & podman @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Label failed with exit code $LASTEXITCODE."
    }
}

if (-not (Get-Command podman -ErrorAction SilentlyContinue)) {
    throw "podman is not installed or not in PATH."
}

& podman info | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw "podman is installed but its engine is unavailable."
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ranvierRoot = (Resolve-Path (Join-Path $scriptDir "..")).Path
$fixtureDir = Join-Path $ranvierRoot "tests/m419-load-soak"
$sourceCommit = (& git -C $ranvierRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $sourceCommit -notmatch "^[0-9a-f]{40}$") {
    throw "Unable to resolve the Ranvier source commit."
}

$dirty = @(& git -C $ranvierRoot status --porcelain=v1 --untracked-files=all)
if ($LASTEXITCODE -ne 0) {
    throw "Unable to inspect the Ranvier worktree."
}
$sourceState = if ($dirty.Count -eq 0) { "clean" } else { "dirty" }
if ($Mode -eq "Full") {
    if ($sourceState -ne "clean") {
        throw "Full evidence requires a clean Ranvier worktree. Commit or remove local changes first."
    }
}

if ([string]::IsNullOrWhiteSpace($OutputRoot)) {
    $OutputRoot = Join-Path $ranvierRoot "target/m419-load-soak"
}
New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null
$resolvedOutputRoot = (Resolve-Path $OutputRoot).Path
$runId = "{0}-{1}" -f (Get-Date -Format "yyyyMMdd-HHmmss"), $Mode.ToLowerInvariant()
$outputDir = Join-Path $resolvedOutputRoot $runId
New-Item -ItemType Directory -Path $outputDir | Out-Null

Invoke-Podman -Label "M419 load/soak image build" -Arguments @(
    "build",
    "--pull=missing",
    "--file", (Join-Path $fixtureDir "Containerfile"),
    "--tag", $Image,
    $fixtureDir
)
$imageInfo = & podman image inspect $Image | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or -not $imageInfo -or $imageInfo[0].Id -notmatch "^[0-9a-f]{64}$") {
    throw "Unable to capture the M419 load/soak image identity."
}
$imageId = $imageInfo[0].Id
$imageDigest = $imageInfo[0].Digest
if ($imageDigest -notmatch "^sha256:[0-9a-f]{64}$") {
    throw "Unable to capture the M419 load/soak image digest."
}

$commonArguments = @(
    "--rm",
    "--security-opt", "label=disable",
    "--volume", "${ranvierRoot}:/workspace:ro",
    "--volume", "ranvier-m419-cargo-registry:/usr/local/cargo/registry",
    "--volume", "ranvier-m419-cargo-git:/usr/local/cargo/git",
    "--volume", "ranvier-m419-target:/cargo-target",
    "--env", "CARGO_TARGET_DIR=/cargo-target",
    "--env", "CARGO_INCREMENTAL=0"
)

# Populate immutable dependency inputs separately so the evidence run itself can
# run without external network access.
$fetchArguments = @("run") + $commonArguments + @(
    $Image,
    "cargo", "fetch", "--locked"
)
Invoke-Podman -Label "M419 Cargo dependency fetch" -Arguments $fetchArguments

$modeArgument = if ($Mode -eq "Full") { "--full" } else { "--quick" }
$gateArguments = @(
    "run",
    "--cpus", "2",
    "--memory", "2g",
    "--pids-limit", "512",
    "--network", "none"
) + $commonArguments + @(
    "--volume", "${outputDir}:/evidence",
    "--env", "CARGO_NET_OFFLINE=true",
    "--env", "RANVIER_RQ10_OUTPUT_ROOT=/evidence",
    "--env", "RANVIER_RQ10_SOURCE_COMMIT=$sourceCommit",
    "--env", "RANVIER_RQ10_IMAGE_ID=$imageId",
    "--env", "RANVIER_RQ10_IMAGE_DIGEST=$imageDigest",
    "--env", "RANVIER_RQ10_SOURCE_STATE=$sourceState",
    $Image,
    "node", "/workspace/scripts/m419_load_soak_gate.mjs",
    $modeArgument,
    "--policy", "/workspace/.ranvier-load-soak-policy.json",
    "--output", "/evidence"
)
Invoke-Podman -Label "M419 $Mode load/soak gate" -Arguments $gateArguments

$resultPath = Join-Path $outputDir "result.json"
if (-not (Test-Path -LiteralPath $resultPath -PathType Leaf)) {
    throw "Gate completed without result.json: $resultPath"
}
if ($Mode -eq "Full") {
    $postCommit = (& git -C $ranvierRoot rev-parse HEAD).Trim()
    $postDirty = @(& git -C $ranvierRoot status --porcelain=v1 --untracked-files=all)
    if ($LASTEXITCODE -ne 0 -or $postCommit -ne $sourceCommit -or $postDirty.Count -ne 0) {
        throw "Ranvier source changed during the Full evidence run; the result is not publishable."
    }
}

Write-Host "M419 $Mode load/soak gate passed."
Write-Host "Evidence: $outputDir"
