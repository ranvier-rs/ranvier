param(
    [string]$EvidenceDir = "..\docs\05_dev_plans\evidence",
    [string]$PostgresImage = "docker.io/library/postgres:16-alpine"
)

$ErrorActionPreference = "Stop"

$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$logPath = Join-Path $EvidenceDir "m119_examples_smoke_$timestamp.log"
New-Item -ItemType Directory -Path $EvidenceDir -Force | Out-Null

function Write-Section {
    param([string]$Title)
    Write-Host ""
    Write-Host "=== $Title ==="
}

function Invoke-FiniteExample {
    param(
        [Parameter(Mandatory = $true)][string]$Package,
        [string]$ExpectText = ""
    )

    Write-Host "[RUN] cargo run -p $Package"
    $output = & cargo run -p $Package 2>&1
    $exitCode = $LASTEXITCODE
    $output | ForEach-Object { Write-Host $_ }

    if ($exitCode -ne 0) {
        throw "Example failed: $Package (exit=$exitCode)"
    }

    if ($ExpectText -and (($output -join "`n") -notmatch [regex]::Escape($ExpectText))) {
        throw "Expected text not found for ${Package}: $ExpectText"
    }
}

function Invoke-ServerStartupExample {
    param(
        [Parameter(Mandatory = $true)][string]$Package,
        [Parameter(Mandatory = $true)][string]$StartupText,
        [int]$TimeoutSec = 25
    )

    $tmpOut = [System.IO.Path]::GetTempFileName()
    $tmpErr = [System.IO.Path]::GetTempFileName()
    try {
        Write-Host "[RUN] cargo run -p $Package (startup-check)"
        $proc = Start-Process `
            -FilePath "cargo" `
            -ArgumentList @("run", "-p", $Package) `
            -NoNewWindow `
            -PassThru `
            -RedirectStandardOutput $tmpOut `
            -RedirectStandardError $tmpErr

        $deadline = (Get-Date).AddSeconds($TimeoutSec)
        $found = $false

        while ((Get-Date) -lt $deadline) {
            $outText = if (Test-Path $tmpOut) { Get-Content $tmpOut -Raw } else { "" }
            $errText = if (Test-Path $tmpErr) { Get-Content $tmpErr -Raw } else { "" }
            $text = "$outText`n$errText"
            if ($text -match [regex]::Escape($StartupText)) {
                $found = $true
                break
            }

            if ($proc.HasExited) {
                break
            }

            Start-Sleep -Milliseconds 500
        }

        if (-not $proc.HasExited) {
            Stop-Process -Id $proc.Id -Force
        }
        Wait-Process -Id $proc.Id -ErrorAction SilentlyContinue

        $capturedOut = if (Test-Path $tmpOut) { Get-Content $tmpOut -Raw } else { "" }
        $capturedErr = if (Test-Path $tmpErr) { Get-Content $tmpErr -Raw } else { "" }
        $captured = "$capturedOut`n$capturedErr"
        if ($captured.Trim()) {
            $captured.Split("`n") | ForEach-Object { Write-Host $_.TrimEnd("`r") }
        }

        if (-not $found) {
            throw "Startup marker not found for ${Package}: $StartupText"
        }
    } finally {
        Remove-Item $tmpOut -ErrorAction SilentlyContinue
        Remove-Item $tmpErr -ErrorAction SilentlyContinue
    }
}

function Invoke-DbExample {
    param([string]$Image)

    $container = "ranvier-m119-postgres-$timestamp"
    $previousDbUrl = $env:DATABASE_URL

    try {
        Write-Host "[RUN] podman run postgres for db-example smoke"
        & podman rm -f $container 2>$null | Out-Null
        & podman run -d --name $container `
            -e POSTGRES_PASSWORD=password `
            -e POSTGRES_DB=ranvier_example `
            -p 55432:5432 `
            $Image | Out-Null

        if ($LASTEXITCODE -ne 0) {
            throw "Failed to start postgres container with image ${Image}"
        }

        Start-Sleep -Seconds 8
        $env:DATABASE_URL = "postgres://postgres:password@127.0.0.1:55432/ranvier_example"

        Invoke-FiniteExample -Package "db-example" -ExpectText "Example completed!"
    } finally {
        if ($null -eq $previousDbUrl) {
            Remove-Item Env:\DATABASE_URL -ErrorAction SilentlyContinue
        } else {
            $env:DATABASE_URL = $previousDbUrl
        }
        & podman stop $container 2>$null | Out-Null
        & podman rm -f $container 2>$null | Out-Null
    }
}

$finitePackages = @(
    "basic-schematic",
    "complex-schematic",
    "typed-state-tree",
    "otel-demo",
    "flat-api-demo",
    "routing-demo",
    "routing-params-demo",
    "session-pattern",
    "otel-concept",
    "synapse-demo",
    "websocket-loop",
    "order-processing-demo",
    "static-build-demo",
    "state-tree-demo",
    "replay-demo",
    "persistence-recovery-demo"
)

$serverPackages = @(
    @{ package = "hello-world"; marker = "Starting server on http://127.0.0.1:3000"; timeout = 25 },
    @{ package = "std-lib-demo"; marker = "Listening on http://127.0.0.1:3000"; timeout = 25 },
    @{ package = "studio-demo"; marker = "Inspector dev page: http://localhost:9000/quick-view"; timeout = 30 },
    @{ package = "fullstack-demo"; marker = "Ranvier Full-Stack Backend (Port 3030)"; timeout = 25 }
)

Start-Transcript -Path $logPath | Out-Null

try {
    Write-Section "M119 Example Smoke Baseline"
    Write-Host "Timestamp: $timestamp"
    Write-Host "Workspace: $(Get-Location)"

    Write-Section "Workspace Compile Check"
    & cargo check --workspace
    if ($LASTEXITCODE -ne 0) {
        throw "cargo check --workspace failed"
    }

    Write-Section "Finite Example Runs"
    foreach ($pkg in $finitePackages) {
        Invoke-FiniteExample -Package $pkg
    }

    Write-Section "Server Startup Example Runs"
    foreach ($item in $serverPackages) {
        Invoke-ServerStartupExample -Package $item.package -StartupText $item.marker -TimeoutSec $item.timeout
    }

    Write-Section "DB Example Run"
    Invoke-DbExample -Image $PostgresImage

    Write-Section "Result"
    Write-Host "M119 example smoke passed."
    Write-Host "Log: $logPath"
} finally {
    Stop-Transcript | Out-Null
}
