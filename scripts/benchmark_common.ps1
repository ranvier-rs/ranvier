# Shared helpers for local benchmark scripts.

$ErrorActionPreference = "Stop"

function Assert-CommandExists {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Name,
        [string]$InstallHint = ""
    )

    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        $message = "Required command '$Name' was not found."
        if ($InstallHint) {
            $message += " $InstallHint"
        }
        throw $message
    }
}

function Wait-TcpPort {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Address,
        [Parameter(Mandatory = $true)]
        [int]$Port,
        [int]$TimeoutSeconds = 20
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        $client = $null
        try {
            $client = [System.Net.Sockets.TcpClient]::new()
            $async = $client.BeginConnect($Address, $Port, $null, $null)
            if ($async.AsyncWaitHandle.WaitOne(500) -and $client.Connected) {
                $client.EndConnect($async)
                return
            }
        }
        catch {
            Start-Sleep -Milliseconds 250
        }
        finally {
            if ($client) {
                $client.Dispose()
            }
        }
    }

    throw "Timed out waiting for TCP port $Address`:$Port to open."
}

function New-EvidencePath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$WorkspaceRoot,
        [Parameter(Mandatory = $true)]
        [string]$Prefix
    )

    $evidenceDir = Join-Path $WorkspaceRoot "docs\05_dev_plans\evidence"
    if (-not (Test-Path $evidenceDir)) {
        New-Item -ItemType Directory -Path $evidenceDir -Force | Out-Null
    }

    $timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
    return Join-Path $evidenceDir "${Prefix}_${timestamp}.log"
}

function Write-BenchmarkMetadata {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string[]]$Lines
    )

    Add-Content -Path $Path -Value ""
    Add-Content -Path $Path -Value "---"
    foreach ($line in $Lines) {
        Add-Content -Path $Path -Value $line
    }
}

function Get-ProcessMemoryMb {
    param(
        [Parameter(Mandatory = $true)]
        [int]$ProcessId
    )

    return [math]::Round(((Get-Process -Id $ProcessId).WorkingSet64 / 1MB), 2)
}

function Get-AutocannonSummary {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    if (-not (Test-Path $Path)) {
        throw "Autocannon output not found: $Path"
    }

    $jsonLine = Get-Content -Path $Path -Encoding UTF8 |
        Where-Object { $_.TrimStart().StartsWith("{") -and $_.TrimEnd().EndsWith("}") } |
        Select-Object -Last 1

    if (-not $jsonLine) {
        throw "Autocannon output did not contain a JSON summary."
    }

    return $jsonLine | ConvertFrom-Json
}

function Assert-AutocannonHealthy {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$Target
    )

    $summary = Get-AutocannonSummary -Path $Path

    if ($summary.errors -ne 0) {
        throw "Autocannon output for $Target reported client errors: $($summary.errors)"
    }
    if ($summary.timeouts -ne 0) {
        throw "Autocannon output for $Target reported timeouts: $($summary.timeouts)"
    }
    if ($summary.non2xx -ne 0) {
        throw "Autocannon output for $Target reported non-2xx responses: $($summary.non2xx)"
    }
    if ($summary.'2xx' -le 0) {
        throw "Autocannon output for $Target did not record any 2xx responses."
    }
}

function Invoke-LoggedProcess {
    param(
        [Parameter(Mandatory = $true)]
        [string]$FilePath,
        [Parameter(Mandatory = $true)]
        [string[]]$ArgumentList,
        [Parameter(Mandatory = $true)]
        [string]$OutputPath
    )

    # This helper overwrites the target output file with the captured stdout/stderr
    # from a single process invocation. Callers that want to preserve prior content
    # should append after this function returns.
    #
    # It also buffers the captured output in memory before writing the final file,
    # which is acceptable for the current benchmark-sized logs but should be
    # revisited if future callers stream much larger outputs.
    $stdoutPath = [System.IO.Path]::GetTempFileName()
    $stderrPath = [System.IO.Path]::GetTempFileName()
    try {
        $process = Start-Process -FilePath $FilePath `
            -ArgumentList $ArgumentList `
            -RedirectStandardOutput $stdoutPath `
            -RedirectStandardError $stderrPath `
            -NoNewWindow `
            -Wait `
            -PassThru

        $stdoutLines = if (Test-Path $stdoutPath) { Get-Content -Path $stdoutPath -Encoding UTF8 } else { @() }
        $stderrLines = if (Test-Path $stderrPath) { Get-Content -Path $stderrPath -Encoding UTF8 } else { @() }

        $allLines = @()
        if ($stdoutLines) { $allLines += $stdoutLines }
        if ($stderrLines) { $allLines += $stderrLines }

        if ($allLines.Count -gt 0) {
            Set-Content -Path $OutputPath -Value $allLines -Encoding UTF8
        }
        else {
            Set-Content -Path $OutputPath -Value @() -Encoding UTF8
        }

        return $process.ExitCode
    }
    finally {
        Remove-Item -Path $stdoutPath, $stderrPath -Force -ErrorAction SilentlyContinue
    }
}
