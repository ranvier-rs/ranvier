param(
    [string]$EvidenceDir = "..\docs\05_dev_plans\evidence"
)

$ErrorActionPreference = "Stop"

$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
$logPath = Join-Path $EvidenceDir "m133_public_api_audit_$timestamp.log"
$jsonPath = Join-Path $EvidenceDir "m133_public_api_audit_$timestamp.json"
New-Item -ItemType Directory -Path $EvidenceDir -Force | Out-Null

$targets = @(
    @{ name = "ranvier-core"; path = "core/src" },
    @{ name = "ranvier-runtime"; path = "runtime/src" },
    @{ name = "ranvier-http"; path = "http/src" },
    @{ name = "ranvier-std"; path = "std/src" },
    @{ name = "ranvier-macros"; path = "macros/src" },
    @{ name = "ranvier"; path = "kit/src" },
    @{ name = "ranvier-auth"; path = "extensions/auth/src" },
    @{ name = "ranvier-guard"; path = "extensions/guard/src" },
    @{ name = "ranvier-openapi"; path = "extensions/openapi/src" },
    @{ name = "ranvier-observe"; path = "extensions/observe/src" },
    @{ name = "ranvier-inspector"; path = "extensions/inspector/src" },
    @{ name = "ranvier-db"; path = "extensions/db/src" },
    @{ name = "ranvier-status"; path = "extensions/status/src" },
    @{ name = "ranvier-synapse"; path = "extensions/synapse/src" }
)

$summary = New-Object System.Collections.Generic.List[object]

Start-Transcript -Path $logPath | Out-Null

try {
    Write-Host "=== M133 Public API Audit Baseline ==="
    Write-Host "Timestamp: $timestamp"
    Write-Host "Workspace: $(Get-Location)"

    foreach ($target in $targets) {
        $sourcePath = $target.path
        if (-not (Test-Path $sourcePath)) {
            Write-Host ""
            Write-Host "[$($target.name)] source path not found: $sourcePath"
            $summary.Add([ordered]@{
                    crate               = $target.name
                    source_path         = $sourcePath
                    public_item_count   = 0
                    deprecated_count    = 0
                    doc_hidden_count    = 0
                    exported_module_count = 0
                    status              = "missing"
                })
            continue
        }

        Write-Host ""
        Write-Host "[$($target.name)] scanning $sourcePath"

        # Public item candidates (excluding visibility-qualified forms such as pub(crate), pub(super), pub(in ...))
        $publicLines = & rg --pcre2 -n --glob '*.rs' '^\s*pub\s+(?!\(|crate|super|in\b)' $sourcePath
        if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 1) {
            throw "rg failed while scanning $sourcePath"
        }

        $deprecatedLines = & rg -n --glob '*.rs' '#\s*\[\s*deprecated' $sourcePath
        if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 1) {
            throw "rg failed while scanning deprecated markers in $sourcePath"
        }

        $docHiddenLines = & rg -n --glob '*.rs' '#\s*\[\s*doc\s*\(\s*hidden' $sourcePath
        if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 1) {
            throw "rg failed while scanning doc(hidden) markers in $sourcePath"
        }

        $exportedModules = & rg -n --glob '*.rs' '^\s*pub\s+mod\s+' $sourcePath
        if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 1) {
            throw "rg failed while scanning public modules in $sourcePath"
        }

        $publicCount = @($publicLines).Count
        $deprecatedCount = @($deprecatedLines).Count
        $docHiddenCount = @($docHiddenLines).Count
        $exportedModuleCount = @($exportedModules).Count

        Write-Host "  public item candidates : $publicCount"
        Write-Host "  #[deprecated] markers  : $deprecatedCount"
        Write-Host "  #[doc(hidden)] markers : $docHiddenCount"
        Write-Host "  pub mod exports        : $exportedModuleCount"

        $summary.Add([ordered]@{
                crate                 = $target.name
                source_path           = $sourcePath
                public_item_count     = $publicCount
                deprecated_count      = $deprecatedCount
                doc_hidden_count      = $docHiddenCount
                exported_module_count = $exportedModuleCount
                status                = "ok"
            })
    }

    $result = [ordered]@{
        timestamp = $timestamp
        generated_by = "scripts/m133_public_api_audit.ps1"
        crate_count = $summary.Count
        crates = $summary
    }

    $result | ConvertTo-Json -Depth 6 | Set-Content -Path $jsonPath -Encoding UTF8

    Write-Host ""
    Write-Host "Result JSON: $jsonPath"
    Write-Host "Log: $logPath"
} finally {
    Stop-Transcript | Out-Null
}
