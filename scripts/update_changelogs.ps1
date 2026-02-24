$content = @"
## [0.10.0] - 2026-02-24

### Added
- Stabilized core Execution and Decision Engine APIs (Gate A).
- Typed fallback execution and error extraction in \`ranvier-core\`.
- \`ranvier-job\` background job scheduling functionality.
- \`ranvier-session\` cache and session management backends.
- Official extensions (\`ranvier-auth\`, \`ranvier-guard\`, \`ranvier-openapi\`) stabilized (Gate B).
- Graceful shutdown and lifecycle hooks.
- Ecosystem reference examples integration (Gate C).

### Changed
- Promoted \`v0.9.x\` APIs to \`v0.10.0\`.
- Transitioned static routing to decoupled \`ranvier-http\`.
- Cleaned up unstable APIs and added proper deprecation tags where necessary.

"@

Get-ChildItem -Path "F:\project\ranvier\ranvier-workspace\ranvier" -Filter "CHANGELOG.md" -Recurse | Where-Object { $_.FullName -notmatch "target" } | ForEach-Object {
    $fileContent = Get-Content $_.FullName -Raw
    if ($fileContent -match "## \[Unreleased\]") {
        $newContent = $fileContent -replace "## \[Unreleased\]", "## [Unreleased]`n`n$content"
        Set-Content -Path $_.FullName -Value $newContent -Encoding UTF8
        Write-Host "Updated $($_.FullName)"
    }
}
