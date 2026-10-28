# “后台继续采样”端到端验收（#14）：用临时设置目录分别以开关关闭/开启启动自己的实例，
# 用 Win32 最小化 N 秒再还原，核对日志里“恢复可见”一行的曲线连续性，并测最小化期间的常驻 CPU。
#   关闭：最小化期间不采样，CPU 曲线应出现断点，最大间隔约等于最小化时长。
#   开启：最小化期间继续采系统指标，曲线无断点，最大间隔约等于采样间隔（1 秒）。
# 只结束自己启动的进程。用法：pwsh scripts/test-background-sampling.ps1 [-HiddenSeconds 8] [-Exe target/release/TaskManagerLite.exe]
param(
    [int]$HiddenSeconds = 8,
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe"
)

$ErrorActionPreference = 'Stop'
$exePath = (Resolve-Path $Exe).Path
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class Win {
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
}
'@

$root = Join-Path ([System.IO.Path]::GetTempPath()) "tml-bgsample-$(Get-Random)"
New-Item -ItemType Directory -Force $root | Out-Null
$failures = New-Object System.Collections.Generic.List[string]
function Check($ok, $msg) {
    if ($ok) { "  通过：$msg" } else { "  失败：$msg"; $script:failures.Add($msg) }
}

# 返回 @{ CpuPercent; Report }：最小化期间占整机的 CPU 百分比，以及还原后日志里的“恢复可见”行。
function Run-Case([bool]$background) {
    $name = if ($background) { 'on' } else { 'off' }
    $settingsDir = Join-Path $root "settings-$name"
    $logDir = Join-Path $root "logs-$name"
    New-Item -ItemType Directory -Force $settingsDir, $logDir | Out-Null
    $json = if ($background) { '{"background_sampling": true}' } else { '{}' }
    Set-Content -Path (Join-Path $settingsDir 'settings.json') -Value $json -Encoding utf8
    $env:TML_SETTINGS_DIR = $settingsDir
    $env:TML_LOG_DIR = $logDir
    $p = Start-Process -FilePath $exePath -ArgumentList '--allow-multiple' -PassThru
    try {
        $h = [IntPtr]::Zero
        for ($i = 0; $i -lt 150 -and $h -eq [IntPtr]::Zero; $i++) {
            Start-Sleep -Milliseconds 100
            $p.Refresh()
            if ($p.HasExited) { throw "进程提前退出，退出码 $($p.ExitCode)" }
            $h = $p.MainWindowHandle
        }
        if ($h -eq [IntPtr]::Zero) { throw '等不到主窗口' }
        Start-Sleep -Seconds 4   # 先在前台积累几秒曲线
        [void][Win]::ShowWindow($h, 6)   # SW_MINIMIZE
        Start-Sleep -Milliseconds 1500    # 可见性轮询周期 300 ms，等它生效并让开销稳定
        if (-not [Win]::IsIconic($h)) { throw '窗口没有最小化' }
        $p.Refresh(); $cpu0 = $p.TotalProcessorTime.TotalMilliseconds
        Start-Sleep -Seconds $HiddenSeconds
        $p.Refresh(); $cpu1 = $p.TotalProcessorTime.TotalMilliseconds
        [void][Win]::ShowWindow($h, 9)   # SW_RESTORE
        Start-Sleep -Seconds 3
        $log = Get-ChildItem $logDir -Filter *.log | Select-Object -First 1
        $report = (Get-Content $log.FullName -Raw -Encoding utf8) -split "`r?`n" | Where-Object { $_ -match '恢复可见' } | Select-Object -Last 1
        $cpu = ($cpu1 - $cpu0) / ($HiddenSeconds * 1000 * [Environment]::ProcessorCount) * 100
        @{ CpuPercent = $cpu; CpuMs = ($cpu1 - $cpu0); Report = $report }
    }
    finally {
        if (-not $p.HasExited) { $p.Kill() }
    }
}

function Field($line, $key) { if ($line -match "$key=(\d+)") { [int]$Matches[1] } else { -1 } }

try {
    "--- 后台采样关闭，最小化 $HiddenSeconds 秒 ---"
    $off = Run-Case $false
    "  日志：$($off.Report)"
    "  最小化期间 CPU 占整机 {0:N3}%（{1:N0} ms CPU 时间）" -f $off.CpuPercent, $off.CpuMs
    Check ($off.Report -ne $null) '还原后日志里有“恢复可见”记录'
    Check ((Field $off.Report '断点') -ge 1) '关闭时曲线在最小化处断开'
    Check ((Field $off.Report '最大间隔') -ge ($HiddenSeconds * 1000 - 1500)) '关闭时最大间隔约等于最小化时长'

    "--- 后台采样开启，最小化 $HiddenSeconds 秒 ---"
    $on = Run-Case $true
    "  日志：$($on.Report)"
    "  最小化期间 CPU 占整机 {0:N3}%（{1:N0} ms CPU 时间）" -f $on.CpuPercent, $on.CpuMs
    Check ($on.Report -ne $null) '还原后日志里有“恢复可见”记录'
    Check ((Field $on.Report '断点') -eq 0) '开启时曲线没有断点'
    $step = Field $on.Report '最大间隔'
    Check ($step -ge 0 -and $step -le 2500) "开启时最大间隔不超过 2.5 秒（实测 $step ms）"
}
finally {
    Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}

if ($failures.Count -gt 0) { "失败 $($failures.Count) 项"; exit 1 }
'全部通过'
