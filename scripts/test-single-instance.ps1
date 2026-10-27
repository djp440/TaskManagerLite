# 单实例限制的自动化验收（Win32 驱动）。只结束本脚本自己启动的进程，日志写入临时 TML_LOG_DIR。
#   1. 实例 A 最小化后再启动 B：B 很快退出，只剩 A，A 已还原并成为前台窗口。
#   2. 模拟提权重启：A 运行时启动 `B --wait-for <A.pid>`，B 不退出也不被挡；关闭 A 后 B 成为唯一实例并显示窗口。
#   3. 已有实例运行时，measure-startup / screenshots / measure-idle 仍能正常工作。
# 用法：pwsh scripts/test-single-instance.ps1 [-Exe target/release/TaskManagerLite.exe]
# 前提：测试时系统里没有别的 TaskManagerLite 实例（否则 A 会被挡掉），运行期间请勿手动切换窗口。
param(
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe"
)

$ErrorActionPreference = 'Stop'
$exePath = (Resolve-Path $Exe).Path

Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class Win {
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
}
'@

$script:failed = 0
$script:started = @()
function Check($ok, $msg) {
    if ($ok) { "  [通过] $msg" } else { "  [失败] $msg"; $script:failed++ }
}
function Instances { @(Get-Process -Name TaskManagerLite -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exePath }) }
function Launch($argList) {
    $p = if ($argList) { Start-Process -FilePath $exePath -ArgumentList $argList -PassThru } else { Start-Process -FilePath $exePath -PassThru }
    $script:started += $p
    $p
}
# 等待进程出现可见的主窗口，返回句柄（超时返回 0）。
function WaitWindow($p, $sec = 10) {
    $end = (Get-Date).AddSeconds($sec)
    while ((Get-Date) -lt $end) {
        $p.Refresh()
        if ($p.HasExited) { return [IntPtr]::Zero }
        if ($p.MainWindowHandle -ne [IntPtr]::Zero -and [Win]::IsWindowVisible($p.MainWindowHandle)) { return $p.MainWindowHandle }
        Start-Sleep -Milliseconds 100
    }
    [IntPtr]::Zero
}
function ForegroundPid { $pid2 = 0; [void][Win]::GetWindowThreadProcessId([Win]::GetForegroundWindow(), [ref]$pid2); [int]$pid2 }
function WaitUntil($cond, $sec) {
    $end = (Get-Date).AddSeconds($sec)
    while ((Get-Date) -lt $end) { if (& $cond) { return $true }; Start-Sleep -Milliseconds 100 }
    [bool](& $cond)
}
function StopAll {
    foreach ($p in $script:started) { try { if (-not $p.HasExited) { $p.Kill(); [void]$p.WaitForExit(3000) } } catch {} }
    $script:started = @()
}

if ((Instances).Count -gt 0) { throw '系统中已有 TaskManagerLite 实例在运行，请先关闭它再运行本测试（脚本不会结束不属于自己的进程）。' }
$env:TML_LOG_DIR = Join-Path ([System.IO.Path]::GetTempPath()) "tml-single-$(Get-Random)"
New-Item -ItemType Directory -Force $env:TML_LOG_DIR | Out-Null

try {
    '== 场景 1：A 最小化后再启动 B =='
    $a = Launch $null
    $ha = WaitWindow $a
    Check ($ha -ne [IntPtr]::Zero) 'A 显示了窗口'
    [void][Win]::ShowWindow($ha, 6)   # SW_MINIMIZE
    Start-Sleep -Milliseconds 800
    Check ([Win]::IsIconic($ha)) 'A 已被最小化'
    $b = Launch $null
    Check ($b.WaitForExit(5000)) 'B 在 5 秒内退出'
    $now = Instances
    Check (($now.Count -eq 1) -and ($now[0].Id -eq $a.Id)) "系统中只剩 A（pid $($a.Id)）"
    Check (WaitUntil { -not [Win]::IsIconic($ha) } 3) 'A 的窗口已还原'
    Check (WaitUntil { (ForegroundPid) -eq $a.Id } 3) "A 的窗口是前台窗口（前台 pid=$(ForegroundPid)）"
    # 还原后可见性轮询应恢复采样，进程仍存活即可，具体 CPU 行为由人工/其他 issue 覆盖。
    Check (-not $a.HasExited) '唤起后 A 仍在运行'
    StopAll

    '== 场景 2：模拟提权重启（B --wait-for A） =='
    $a = Launch $null
    $ha = WaitWindow $a
    Check ($ha -ne [IntPtr]::Zero) 'A 显示了窗口'
    $b = Launch @('--wait-for', "$($a.Id)")
    Start-Sleep -Seconds 2
    Check (-not $b.HasExited) 'A 仍在时 B 没有退出（没被挡掉）'
    $b.Refresh()
    Check ($b.MainWindowHandle -eq [IntPtr]::Zero) 'A 仍在时 B 还没显示窗口（在等待）'
    $a.Kill(); [void]$a.WaitForExit(3000)
    $hb = WaitWindow $b
    Check ($hb -ne [IntPtr]::Zero) 'A 退出后 B 显示了窗口'
    $now = Instances
    Check (($now.Count -eq 1) -and ($now[0].Id -eq $b.Id)) "B 成为唯一实例（pid $($b.Id)）"
    # B 此时是唯一实例，再启动 C 应被挡掉。
    $c = Launch $null
    Check ($c.WaitForExit(5000)) '之后再启动 C 仍会被挡掉'
    StopAll

    '== 场景 3：已有实例运行时开发脚本仍可用 =='
    $a = Launch $null
    [void](WaitWindow $a)
    $shots = Join-Path ([System.IO.Path]::GetTempPath()) "tml-single-shots-$(Get-Random)"
    $pwsh = (Get-Command pwsh).Source
    $out = & $pwsh -NoProfile -File "$PSScriptRoot/measure-startup.ps1" -Runs 2 -Exe $exePath 2>&1 | Out-String
    Check (($LASTEXITCODE -eq 0) -and ($out -match '首个快照帧 中位数')) 'measure-startup.ps1 正常'
    $out = & $pwsh -NoProfile -File "$PSScriptRoot/screenshots.ps1" -Out $shots -Exe $exePath 2>&1 | Out-String
    Check (($LASTEXITCODE -eq 0) -and (@(Get-ChildItem $shots -Filter *.png -ErrorAction SilentlyContinue).Count -gt 0)) 'screenshots.ps1 正常并生成了 PNG'
    $out = & $pwsh -NoProfile -File "$PSScriptRoot/measure-idle.ps1" -Seconds 3 -Exe $exePath 2>&1 | Out-String
    Check (($LASTEXITCODE -eq 0) -and ($out -match 'CPU 占整机') -and ($out -match '首个快照帧')) 'measure-idle.ps1 正常'
    Check (-not $a.HasExited) '整个过程中 A 一直在运行'
    Remove-Item -Recurse -Force $shots -ErrorAction SilentlyContinue
}
finally {
    StopAll
    Remove-Item -Recurse -Force $env:TML_LOG_DIR -ErrorAction SilentlyContinue
}

if ($script:failed -gt 0) { "失败 $script:failed 项"; exit 1 }
'全部通过'
