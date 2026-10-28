# 开机启动参数 --autostart 的端到端验收（#16）。全部用自己启动的实例，只结束自己的进程；不碰注册表的 Run 键。
#   - 用 TML_INSTANCE_ID 隔离单实例对象名、临时 TML_SETTINGS_DIR / TML_LOG_DIR，机器上已有真实实例也不受影响。
# 验证：开启“关闭时最小化到托盘”时不显示任何窗口、只有托盘图标，托盘单击后窗口出现；
#       迷你模式下从托盘唤起的是迷你窗口；未开启托盘时窗口最小化启动；主题设置原样写回。
# 用法：pwsh scripts/test-autostart.ps1 [-Exe target/release/TaskManagerLite.exe]
param([string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe")

$ErrorActionPreference = 'Stop'
Add-Type @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class Win {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    // 属于 pid 的顶层窗口：cls 为空时取界面窗口（winit 的窗口类名，完整窗口与迷你窗口都是它），visibleOnly 时只取可见的。
    public static List<IntPtr> Windows(uint pid, string cls, bool visibleOnly) {
        var r = new List<IntPtr>();
        EnumWindows((h, l) => {
            uint p; GetWindowThreadProcessId(h, out p);
            var sb = new StringBuilder(128); GetClassName(h, sb, 128);
            var name = sb.ToString();
            // PowerShell 传 $null 给 string 参数时会变成空字符串。
            bool match = string.IsNullOrEmpty(cls) ? name == "Window Class" : name == cls;
            if (p == pid && match && (!visibleOnly || IsWindowVisible(h))) r.Add(h);
            return true;
        }, IntPtr.Zero);
        return r;
    }
}
'@
[void][Win]::SetProcessDPIAware()

$exePath = (Resolve-Path $Exe).Path
$root = Join-Path ([System.IO.Path]::GetTempPath()) "tml-autostart-test-$(Get-Random)"
New-Item -ItemType Directory -Force $root | Out-Null
$WM_APP_CALLBACK = 0x8001; $WM_LBUTTONUP = 0x0202; $WM_COMMAND = 0x0111; $WM_CLOSE = 0x0010
$CMD_EXIT = 1002
$failures = New-Object System.Collections.Generic.List[string]
$started = New-Object System.Collections.Generic.List[object]
function Check($ok, $msg) {
    if ($ok) { "  通过：$msg" } else { "  失败：$msg"; $script:failures.Add($msg) }
}
function WaitUntil([scriptblock]$cond, $sec) {
    $end = (Get-Date).AddSeconds($sec)
    while ((Get-Date) -lt $end) { if (& $cond) { return $true }; Start-Sleep -Milliseconds 100 }
    [bool](& $cond)
}

# 以 --autostart 启动一个隔离的实例。返回 @{ Process; Settings; LogDir }。
function Start-Auto($json) {
    $id = "auto$(Get-Random)"
    $settingsDir = Join-Path $root "settings-$id"; $logDir = Join-Path $root "logs-$id"
    New-Item -ItemType Directory -Force $settingsDir, $logDir | Out-Null
    Set-Content -Path (Join-Path $settingsDir 'settings.json') -Value $json -Encoding utf8
    $env:TML_INSTANCE_ID = $id; $env:TML_SETTINGS_DIR = $settingsDir; $env:TML_LOG_DIR = $logDir
    $p = Start-Process -FilePath $exePath -ArgumentList '--autostart' -PassThru
    $script:started.Add($p)
    @{ Process = $p; Settings = (Join-Path $settingsDir 'settings.json'); LogDir = $logDir }
}
function Log-Text($app) {
    $f = Get-ChildItem $app.LogDir -Filter *.log | Select-Object -First 1
    if ($f) { Get-Content $f.FullName -Raw -Encoding utf8 } else { '' }
}
function Tray-Of($app) {
    $t = [Win]::Windows([uint32]$app.Process.Id, 'TaskManagerLiteTray', $false)
    if ($t.Count -gt 0) { $t[0] } else { [IntPtr]::Zero }
}
function Visible-Windows($app) { @([Win]::Windows([uint32]$app.Process.Id, $null, $true)) }
function Logical-Width($h) {
    $r = New-Object Win+RECT
    [void][Win]::GetClientRect($h, [ref]$r)
    ($r.R - $r.L) / ([Win]::GetDpiForWindow($h) / 96.0)
}
function Exit-ViaTray($app, $tray) {
    [void][Win]::PostMessage($tray, $WM_COMMAND, [IntPtr]$CMD_EXIT, [IntPtr]::Zero)
    Check ($app.Process.WaitForExit(8000)) '托盘菜单「退出」：进程结束'
}

try {
    '=== 开启关闭到托盘：--autostart 不显示窗口，只有托盘图标 ==='
    $a = Start-Auto '{"close_to_tray": true, "theme": "dark"}'
    Check (WaitUntil { (Tray-Of $a) -ne [IntPtr]::Zero } 10) '托盘消息窗口已创建'
    Start-Sleep -Milliseconds 1500
    Check (-not $a.Process.HasExited) '进程常驻'
    Check ((Visible-Windows $a).Count -eq 0) "没有可见窗口（实际 $((Visible-Windows $a).Count) 个）"
    Check ((Log-Text $a) -match '开机启动 隐藏到托盘') '日志记录「开机启动 隐藏到托盘」'
    $tray = Tray-Of $a
    [void][Win]::PostMessage($tray, $WM_APP_CALLBACK, [IntPtr]1, [IntPtr]$WM_LBUTTONUP)
    Check (WaitUntil { (Visible-Windows $a).Count -gt 0 } 5) '托盘单击后窗口出现'
    $w = Visible-Windows $a
    if ($w.Count -gt 0) {
        Check (-not [Win]::IsIconic($w[0])) '唤起的窗口不是最小化状态'
        Check ((Logical-Width $w[0]) -gt 700) "唤起的是完整窗口（逻辑宽 $([int](Logical-Width $w[0]))）"
    }
    Exit-ViaTray $a $tray
    $s = Get-Content $a.Settings -Raw | ConvertFrom-Json
    Check ($s.theme -eq 'dark' -and $s.close_to_tray -eq $true) "主题与托盘设置原样写回（theme=$($s.theme)）"

    '=== 迷你模式 + 关闭到托盘：--autostart 不显示窗口，唤起的是迷你窗口 ==='
    $m = Start-Auto '{"close_to_tray": true, "mini": {"active": true, "x": 200, "y": 160}}'
    Check (WaitUntil { (Tray-Of $m) -ne [IntPtr]::Zero } 10) '托盘消息窗口已创建'
    Start-Sleep -Milliseconds 1500
    Check ((Visible-Windows $m).Count -eq 0) "没有可见窗口（实际 $((Visible-Windows $m).Count) 个）"
    $tray = Tray-Of $m
    [void][Win]::PostMessage($tray, $WM_APP_CALLBACK, [IntPtr]1, [IntPtr]$WM_LBUTTONUP)
    Check (WaitUntil { (Visible-Windows $m).Count -gt 0 } 5) '托盘单击后窗口出现'
    $w = Visible-Windows $m
    if ($w.Count -gt 0) {
        $lw = Logical-Width $w[0]
        Check ([math]::Abs($lw - 250) -le 2 -and $w.Count -eq 1) "唤起的是迷你窗口（逻辑宽 $([int]$lw)，可见窗口 $($w.Count) 个）"
    }
    Exit-ViaTray $m $tray
    $s = Get-Content $m.Settings -Raw | ConvertFrom-Json
    Check ($s.mini.active -eq $true -and $s.mini.x -eq 200) "退出时仍是迷你模式，位置保留（active=$($s.mini.active) x=$($s.mini.x)）"

    '=== 未开启关闭到托盘：--autostart 最小化启动 ==='
    $b = Start-Auto '{"close_to_tray": false, "theme": "light"}'
    Check (WaitUntil { (Visible-Windows $b).Count -gt 0 -and [Win]::IsIconic((Visible-Windows $b)[0]) } 10) '窗口最小化显示在任务栏'
    Start-Sleep -Milliseconds 1000
    Check ((Tray-Of $b) -eq [IntPtr]::Zero) '未开启时不创建托盘图标'
    Check ((Log-Text $b) -match '开机启动 最小化') '日志记录「开机启动 最小化」'
    $w = Visible-Windows $b
    if ($w.Count -gt 0) { [void][Win]::PostMessage($w[0], $WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero) }
    Check ($b.Process.WaitForExit(8000)) 'WM_CLOSE：进程退出'
    $s = Get-Content $b.Settings -Raw | ConvertFrom-Json
    Check ($s.theme -eq 'light') "主题原样写回（theme=$($s.theme)）"
    # 最小化期间退出不应把最小化的位置当成窗口位置保存。
    Check ($null -eq $s.window -or $s.window.x -gt -10000) "未保存最小化时的位置（window=$($s.window | ConvertTo-Json -Compress)）"
}
finally {
    foreach ($p in $started) { try { if (-not $p.HasExited) { $p.Kill() } } catch {} }
    Remove-Item Env:TML_INSTANCE_ID, Env:TML_SETTINGS_DIR, Env:TML_LOG_DIR -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}

if ($failures.Count -gt 0) { "失败 $($failures.Count) 项"; exit 1 }
'全部通过'
