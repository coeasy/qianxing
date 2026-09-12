[CmdletBinding()]
param(
    [string]$Config = (Join-Path $PSScriptRoot "qianxing.runtime.production.example.json"),
    [string]$Binary = (Join-Path $PSScriptRoot "..\target\release\qx-cli.exe"),
    [switch]$AllowUnmanagedRoles
)

$ErrorActionPreference = "Stop"

$configPath = (Resolve-Path -LiteralPath $Config).Path
$binaryPath = (Resolve-Path -LiteralPath $Binary).Path
$workDir = (Get-Location).Path
$runtime = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json

& $binaryPath runtime-check $configPath
if ($LASTEXITCODE -ne 0) {
    throw "runtime-check failed; no child process was started"
}

$dataDir = [string]$runtime.storage.data_dir
if (-not [IO.Path]::IsPathRooted($dataDir)) {
    $dataDir = Join-Path $workDir $dataDir
}
$logDir = Join-Path $dataDir "process-logs"
New-Item -ItemType Directory -Force -Path $logDir | Out-Null

$launches = @()
$unmanaged = @()
foreach ($worker in $runtime.workers) {
    if (-not [bool]$worker.enabled) {
        continue
    }
    $role = [string]$worker.role
    switch ($role) {
        "api" {
            $launches += [pscustomobject]@{ Id = $worker.id; Args = @("serve", $configPath) }
        }
        "market_data" { $launches += [pscustomobject]@{ Id = $worker.id; Args = @("binance-worker", $configPath, $worker.id) } }
        "user_stream" { $launches += [pscustomobject]@{ Id = $worker.id; Args = @("binance-worker", $configPath, $worker.id) } }
        "execution" {
            if ([string]$worker.venue_id -eq "paper") {
                $launches += [pscustomobject]@{ Id = $worker.id; Args = @("paper-worker", $configPath, $worker.id) }
            } else {
                $launches += [pscustomobject]@{ Id = $worker.id; Args = @("binance-worker", $configPath, $worker.id) }
            }
        }
        "reconciler" { $launches += [pscustomobject]@{ Id = $worker.id; Args = @("binance-worker", $configPath, $worker.id) } }
        "scheduler" { $launches += [pscustomobject]@{ Id = $worker.id; Args = @("scheduler-worker", $configPath, $worker.id) } }
        "strategy" { $launches += [pscustomobject]@{ Id = $worker.id; Args = @("strategy-worker", $configPath, $worker.id) } }
        default { $unmanaged += $worker.id }
    }
}

if ($unmanaged.Count -gt 0 -and -not $AllowUnmanagedRoles) {
    throw "enabled roles have no qx-cli process entrypoint: $($unmanaged -join ', '); use the dedicated scheduler/strategy supervisor or pass -AllowUnmanagedRoles"
}

$children = @()
try {
    foreach ($launch in $launches) {
        $stdout = Join-Path $logDir "$($launch.Id).out.log"
        $stderr = Join-Path $logDir "$($launch.Id).err.log"
        $process = Start-Process -FilePath $binaryPath `
            -ArgumentList $launch.Args `
            -WorkingDirectory $workDir `
            -RedirectStandardOutput $stdout `
            -RedirectStandardError $stderr `
            -WindowStyle Hidden `
            -PassThru
        $children += $process
        Write-Host "started $($launch.Id) pid=$($process.Id)"
    }
    while ($true) {
        Start-Sleep -Seconds 1
        foreach ($process in $children) {
            $process.Refresh()
            if ($process.HasExited) {
                throw "managed worker pid=$($process.Id) exited with code $($process.ExitCode)"
            }
        }
    }
}
finally {
    foreach ($process in $children) {
        $process.Refresh()
        if (-not $process.HasExited) {
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        }
    }
}
