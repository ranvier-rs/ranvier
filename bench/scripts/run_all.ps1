# =============================================================================
# run_all.ps1 -- M218 Performance Benchmark Runner
# =============================================================================
#
# This script automates the build and criterion phases of the Ranvier benchmark
# suite. HTTP load testing (wrk/oha) requires servers to be running in separate
# processes, so this script prints instructions for that phase.
#
# Usage:
#   pwsh -File run_all.ps1
#   pwsh -File run_all.ps1 -SkipBuild
#   pwsh -File run_all.ps1 -CriterionOnly
#
# =============================================================================

param(
    [switch]$SkipBuild,
    [switch]$CriterionOnly
)

$ErrorActionPreference = "Stop"
$BenchRoot = Split-Path -Parent $PSScriptRoot
$WorkspaceRoot = Split-Path -Parent $BenchRoot

Write-Host ""
Write-Host "=============================================" -ForegroundColor Cyan
Write-Host "  Ranvier Performance Benchmark Suite (M218)" -ForegroundColor Cyan
Write-Host "=============================================" -ForegroundColor Cyan
Write-Host ""
Write-Host "Workspace root: $WorkspaceRoot"
Write-Host "Bench crate:    $BenchRoot"
Write-Host ""

# -----------------------------------------------------------------------------
# Phase 1: Build all release binaries
# -----------------------------------------------------------------------------

if (-not $SkipBuild -and -not $CriterionOnly) {
    Write-Host "--- Phase 1: Building all benchmark binaries (release) ---" -ForegroundColor Yellow
    Write-Host ""

    $binaries = @(
        # Ranvier servers
        "scenario1_server",
        "scenario2_server",
        "scenario3_server",
        "scenario4_server",
        # Axum comparison servers
        "axum_scenario1",
        "axum_scenario2",
        "axum_scenario3",
        "axum_scenario4",
        # Actix-web comparison servers
        "actix_scenario1",
        "actix_scenario2",
        "actix_scenario3"
    )

    foreach ($bin in $binaries) {
        Write-Host "  Building: $bin" -ForegroundColor Gray
        cargo build --release --bin $bin --manifest-path "$BenchRoot\Cargo.toml" 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) {
            Write-Host "  FAILED: $bin" -ForegroundColor Red
            cargo build --release --bin $bin --manifest-path "$BenchRoot\Cargo.toml"
            exit 1
        }
    }

    Write-Host ""
    Write-Host "  All binaries built successfully." -ForegroundColor Green
    Write-Host ""
}
else {
    Write-Host "--- Phase 1: Build skipped (flag) ---" -ForegroundColor DarkGray
    Write-Host ""
}

# -----------------------------------------------------------------------------
# Phase 2: Run criterion microbenchmarks
# -----------------------------------------------------------------------------

Write-Host "--- Phase 2: Criterion Microbenchmarks ---" -ForegroundColor Yellow
Write-Host ""
Write-Host "  Running: cargo bench -p ranvier-bench" -ForegroundColor Gray
Write-Host ""

Push-Location $WorkspaceRoot
try {
    cargo bench -p ranvier-bench
    if ($LASTEXITCODE -ne 0) {
        Write-Host "  Criterion benchmarks failed." -ForegroundColor Red
        exit 1
    }
}
finally {
    Pop-Location
}

Write-Host ""
Write-Host "  Criterion reports: $WorkspaceRoot\target\criterion\" -ForegroundColor Green
Write-Host ""

if ($CriterionOnly) {
    Write-Host "--- CriterionOnly mode -- skipping HTTP instructions ---" -ForegroundColor DarkGray
    exit 0
}

# -----------------------------------------------------------------------------
# Phase 3: HTTP Load Test Instructions
# -----------------------------------------------------------------------------

Write-Host "--- Phase 3: HTTP Load Testing (manual) ---" -ForegroundColor Yellow
Write-Host ""
Write-Host "  The following servers must be started in separate terminals." -ForegroundColor White
Write-Host "  After all servers are running, execute the wrk/oha commands below." -ForegroundColor White
Write-Host ""

# -- Server start commands --

Write-Host "  ---- Start Servers ----" -ForegroundColor Cyan
Write-Host ""

$servers = @(
    @{ Name = "Ranvier  Scenario 1 (Hello World)";    Bin = "scenario1_server";  Port = 3000 },
    @{ Name = "Ranvier  Scenario 2 (Auth Flow)";      Bin = "scenario2_server";  Port = 3001 },
    @{ Name = "Ranvier  Scenario 3 (Workflow)";        Bin = "scenario3_server";  Port = 3002 },
    @{ Name = "Axum     Scenario 1 (Hello World)";     Bin = "axum_scenario1";    Port = 4000 },
    @{ Name = "Axum     Scenario 2 (Auth Flow)";       Bin = "axum_scenario2";    Port = 4001 },
    @{ Name = "Axum     Scenario 3 (Workflow)";         Bin = "axum_scenario3";    Port = 4002 },
    @{ Name = "Actix    Scenario 1 (Hello World)";     Bin = "actix_scenario1";   Port = 5000 },
    @{ Name = "Actix    Scenario 2 (Auth Flow)";       Bin = "actix_scenario2";   Port = 5001 },
    @{ Name = "Actix    Scenario 3 (Workflow)";         Bin = "actix_scenario3";   Port = 5002 }
)

foreach ($s in $servers) {
    Write-Host ("  # {0} (:{1})" -f $s.Name, $s.Port) -ForegroundColor DarkGray
    Write-Host ("  cargo run --release --bin {0}" -f $s.Bin)
    Write-Host ""
}

# -- Load test commands --

Write-Host "  ---- Run Load Tests (wrk) ----" -ForegroundColor Cyan
Write-Host ""
Write-Host "  # Scenario 1: Hello World"
Write-Host "  wrk -t12 -c400 -d30s http://127.0.0.1:3000/"
Write-Host "  wrk -t12 -c400 -d30s http://127.0.0.1:4000/"
Write-Host "  wrk -t12 -c400 -d30s http://127.0.0.1:5000/"
Write-Host ""
Write-Host "  # Scenario 2: Auth Flow (generate token first)"
Write-Host '  $TOKEN = cargo run --release --bin scenario2_server -- gen-token'
Write-Host '  oha -c 400 -z 30s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:3001/protected'
Write-Host '  oha -c 400 -z 30s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:4001/protected'
Write-Host '  oha -c 400 -z 30s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:5001/protected'
Write-Host ""
Write-Host "  # Scenario 3: Multi-step Workflow"
Write-Host "  wrk -t12 -c400 -d30s http://127.0.0.1:3002/workflow"
Write-Host "  wrk -t12 -c400 -d30s http://127.0.0.1:4002/workflow"
Write-Host "  wrk -t12 -c400 -d30s http://127.0.0.1:5002/workflow"
Write-Host ""

# -- Alternative: oha --

Write-Host "  ---- Alternative: oha ----" -ForegroundColor Cyan
Write-Host ""
Write-Host "  # Scenario 1"
Write-Host "  oha -c 400 -z 30s http://127.0.0.1:3000/"
Write-Host "  oha -c 400 -z 30s http://127.0.0.1:4000/"
Write-Host "  oha -c 400 -z 30s http://127.0.0.1:5000/"
Write-Host ""
Write-Host "  # Scenario 3"
Write-Host "  oha -c 400 -z 30s http://127.0.0.1:3002/workflow"
Write-Host "  oha -c 400 -z 30s http://127.0.0.1:4002/workflow"
Write-Host "  oha -c 400 -z 30s http://127.0.0.1:5002/workflow"
Write-Host ""

# -- Port summary --

Write-Host "  ---- Port Allocation ----" -ForegroundColor Cyan
Write-Host ""
Write-Host "  Ranvier    3000 - 3003"
Write-Host "  Axum       4000 - 4003"
Write-Host "  Actix-web  5000 - 5003"
Write-Host ""

Write-Host "=============================================" -ForegroundColor Cyan
Write-Host "  Benchmark suite complete." -ForegroundColor Green
Write-Host "  Fill results into: docs/discussion/238_performance_benchmark_v0_23.md" -ForegroundColor Green
Write-Host "=============================================" -ForegroundColor Cyan
Write-Host ""
