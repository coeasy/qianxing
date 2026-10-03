[CmdletBinding()]
param(
    [string]$Config = (Join-Path $PSScriptRoot "qianxing.runtime.production.example.json"),
    [string]$Binary = (Join-Path $PSScriptRoot "..\target\release\qx-cli.exe"),
    [switch]$AllowUnmanagedRoles
)

# 角色到进程入口的映射、按 worker 分离的进程日志、以及"任一子进程退出就停掉其余子进程"
# 的 fail-fast 生命周期都只有一份实现：`qx-cli supervise`（crates/qx-orchestrator 的 plan_workers）。
# 本文件此前自带一份 PowerShell 副本，那份副本与 plan_workers 漂移过四处（V13 R2 #192）：
# CCXT 的 endpoint 只对 spread_recovery 一个角色生效、paper 判定用精确字符串而不是 VenueFamily 归一、
# plan_workers 会拒绝的拓扑在这里被静默派给币安那条线、有内建入口的 outbox_relay 与
# event_consumer 角色在这里被当成不可托管。副本还会在没有任何可托管 worker 时进入不退出循环。
# 删掉副本，只保留启动器与 runtime-check 前置闸门。

$ErrorActionPreference = "Stop"

$configPath = (Resolve-Path -LiteralPath $Config).Path
$binaryPath = (Resolve-Path -LiteralPath $Binary).Path

& $binaryPath runtime-check $configPath
if ($LASTEXITCODE -ne 0) {
    throw "runtime-check failed; no child process was started"
}

$supervise = @("supervise", $configPath)
if ($AllowUnmanagedRoles) {
    $supervise += "--allow-unmanaged-roles"
}
& $binaryPath @supervise
exit $LASTEXITCODE
