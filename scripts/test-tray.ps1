# 托盘常驻端到端验收（#8）。全部用自己启动的实例，只结束自己的进程。
#   - 用 TML_INSTANCE_ID 隔离单实例对象名、临时 TML_SETTINGS_DIR / TML_LOG_DIR，机器上已有真实实例也不受影响。
#   - 主实例不带 --allow-multiple（才会监听唤起请求），设置里 close_to_tray=true。
#   - 托盘消息窗口（类名 TaskManagerLiteTray）收到 PostMessage 的回调消息，等价于真实的托盘点击。
# 验证：托盘图标已登记（Shell_NotifyIconGetRect）；左键单击隐藏/显示；WM_CLOSE 后进程仍在且窗口隐藏；
#       再次启动 exe 唤起隐藏的窗口；右键菜单可弹出（截图）并可取消；菜单「退出」结束进程；
#       隐藏到托盘期间的常驻 CPU（后台采样关闭/开启）与恢复后的曲线连续性。
# 用法：pwsh scripts/test-tray.ps1 [-HiddenSeconds 8] [-Out <截图目录>] [-Exe target/release/TaskManagerLite.exe]
param(
    [int]$HiddenSeconds = 8,
    [string]$Out = (Join-Path ([System.IO.Path]::GetTempPath()) 'tml-tray'),
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe"
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
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
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [StructLayout(LayoutKind.Sequential)] public struct NOTIFYICONIDENTIFIER {
        public uint cbSize; public IntPtr hWnd; public uint uID; public Guid guidItem;
    }
    [DllImport("shell32.dll")] static extern int Shell_NotifyIconGetRect(ref NOTIFYICONIDENTIFIER id, out RECT r);
    public static bool Visible(IntPtr h) { return IsWindowVisible(h); }
    // 属于 pid、指定类名的顶层窗口（含不可见的）。
    public static List<IntPtr> ByClass(uint pid, string cls, bool visibleOnly) {
        var r = new List<IntPtr>();
        EnumWindows((h, l) => {
            uint p; GetWindowThreadProcessId(h, out p);
            var sb = new StringBuilder(128); GetClassName(h, sb, 128);
            if (p == pid && sb.ToString() == cls && (!visibleOnly || IsWindowVisible(h))) r.Add(h);
            return true;
        }, IntPtr.Zero);
        return r;
    }
    // 托盘图标在任务栏上的矩形；图标未登记时返回非零 HRESULT。
    public static int IconRect(IntPtr hwnd, out RECT r) {
        var id = new NOTIFYICONIDENTIFIER();
        id.cbSize = (uint)Marshal.SizeOf(typeof(NOTIFYICONIDENTIFIER));
        id.hWnd = hwnd; id.uID = 1;
        return Shell_NotifyIconGetRect(ref id, out r);
    }
}
'@
[void][Win]::SetProcessDPIAware()

$exePath = (Resolve-Path $Exe).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$root = Join-Path ([System.IO.Path]::GetTempPath()) "tml-tray-test-$(Get-Random)"
New-Item -ItemType Directory -Force $root | Out-Null
$WM_APP_CALLBACK = 0x8001; $WM_LBUTTONUP = 0x0202; $WM_RBUTTONUP = 0x0205
$WM_COMMAND = 0x0111; $WM_CLOSE = 0x0010; $WM_CANCELMODE = 0x001F
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

# 启动主实例（单实例模式，close_to_tray=true）。返回 @{ Process; Main; Tray; Log; Env }。
function Start-Primary([bool]$background, [bool]$closeToTray = $true) {
    $id = "tray$(Get-Random)"
    $settingsDir = Join-Path $root "settings-$id"; $logDir = Join-Path $root "logs-$id"
    New-Item -ItemType Directory -Force $settingsDir, $logDir | Out-Null
    $json = '{"close_to_tray": ' + $closeToTray.ToString().ToLower() + ', "background_sampling": ' + $background.ToString().ToLower() + '}'
    Set-Content -Path (Join-Path $settingsDir 'settings.json') -Value $json -Encoding utf8
    $env:TML_INSTANCE_ID = $id; $env:TML_SETTINGS_DIR = $settingsDir; $env:TML_LOG_DIR = $logDir
    $p = Start-Process -FilePath $exePath -PassThru
    $script:started.Add($p)
    $main = [IntPtr]::Zero
    for ($i = 0; $i -lt 100 -and $main -eq [IntPtr]::Zero; $i++) {
        Start-Sleep -Milliseconds 100
        $p.Refresh()
        if ($p.HasExited) { throw "主实例提前退出，退出码 $($p.ExitCode)" }
        $main = $p.MainWindowHandle
    }
    if ($main -eq [IntPtr]::Zero) { throw '等不到主窗口' }
    $tray = [IntPtr]::Zero
    [void](WaitUntil { $t = [Win]::ByClass([uint32]$p.Id, 'TaskManagerLiteTray', $false); if ($t.Count -gt 0) { $script:tmp = $t[0]; $true } else { $false } } 10)
    if ($script:tmp) { $tray = $script:tmp; $script:tmp = $null }
    @{ Process = $p; Main = $main; Tray = $tray; LogDir = $logDir; Id = $id }
}
function Click-Tray($app, $msg) {
    [void][Win]::PostMessage($app.Tray, $WM_APP_CALLBACK, [IntPtr]1, [IntPtr]$msg)
}
function Log-Text($app) {
    $f = Get-ChildItem $app.LogDir -Filter *.log | Select-Object -First 1
    if ($f) { Get-Content $f.FullName -Raw -Encoding utf8 } else { '' }
}
function Stop-Own($app) {
    if (-not $app.Process.HasExited) { $app.Process.Kill(); [void]$app.Process.WaitForExit(3000) }
}

try {
    '=== 功能验收（后台采样关闭）==='
    $a = Start-Primary $false
    Check ($a.Tray -ne [IntPtr]::Zero) '托盘消息窗口已创建（首个快照之后）'
    # 资源管理器异步处理 NIM_ADD：轮询直到能查到图标矩形。S_OK 与 S_FALSE（图标在溢出区）都说明已登记。
    $r = New-Object Win+RECT; $hr = -1
    [void](WaitUntil { $script:hr = [Win]::IconRect($a.Tray, [ref]$script:r); $script:hr -ge 0 -and ($script:r.R - $script:r.L) -gt 0 } 5)
    Check ($hr -ge 0 -and ($r.R - $r.L) -gt 0) ("托盘图标已登记（Shell_NotifyIconGetRect HRESULT=0x{0:X}，矩形 {1},{2}-{3},{4}）" -f $hr, $r.L, $r.T, $r.R, $r.B)
    Check ([Win]::Visible($a.Main) -and -not [Win]::IsIconic($a.Main)) '启动后主窗口可见'
    # 截取图标所在的任务栏区域（图标可能在溢出区里，此时截到的是 ^ 按钮；仅供人眼查看）。
    if ($hr -ge 0) {
        $pad = 24
        $cw = ($r.R - $r.L) + 2 * $pad; $ch = ($r.B - $r.T) + 2 * $pad
        $shot = New-Object System.Drawing.Bitmap $cw, $ch
        $sg = [System.Drawing.Graphics]::FromImage($shot)
        $sg.CopyFromScreen($r.L - $pad, $r.T - $pad, 0, 0, (New-Object System.Drawing.Size $cw, $ch))
        $sg.Dispose()
        $shotFile = Join-Path $Out '托盘图标区域.png'
        $shot.Save($shotFile, [System.Drawing.Imaging.ImageFormat]::Png); $shot.Dispose()
        "  托盘图标区域截图 ${cw}x${ch} -> $shotFile"
    }

    Click-Tray $a $WM_LBUTTONUP
    Check (WaitUntil { -not [Win]::Visible($a.Main) } 3) '托盘左键单击：窗口隐藏'
    $a.Process.Refresh()
    Check (-not $a.Process.HasExited) '隐藏后进程仍在'
    Check ((Log-Text $a) -match '隐藏到托盘 提示=.*CPU \d+%.*内存 \d+%') '隐藏时托盘提示包含 CPU 与内存读数（见日志）'
    (Log-Text $a) -split "`r?`n" | Where-Object { $_ -match '隐藏到托盘' } | Select-Object -Last 1 | ForEach-Object { "  日志：$_" }

    Click-Tray $a $WM_LBUTTONUP
    Check (WaitUntil { [Win]::Visible($a.Main) -and -not [Win]::IsIconic($a.Main) } 3) '托盘左键再次单击：窗口显示'

    [void][Win]::PostMessage($a.Main, $WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero)
    Check (WaitUntil { -not [Win]::Visible($a.Main) } 3) 'WM_CLOSE：窗口隐藏到托盘'
    Start-Sleep -Milliseconds 800
    $a.Process.Refresh()
    Check (-not $a.Process.HasExited) 'WM_CLOSE 后进程仍在'
    Check ([Win]::ByClass([uint32]$a.Process.Id, 'TaskManagerLiteTray', $false).Count -eq 1) 'WM_CLOSE 后托盘窗口仍在'

    $t0 = Get-Date
    $b = Start-Process -FilePath $exePath -PassThru   # 第二个实例，同样的 TML_INSTANCE_ID
    $started.Add($b)
    Check ($b.WaitForExit(5000)) '再次启动 exe：新进程很快退出（单实例）'
    Check (WaitUntil { [Win]::Visible($a.Main) -and -not [Win]::IsIconic($a.Main) } 3) '再次启动 exe：隐藏在托盘的窗口被唤起'
    ("  从启动第二个进程到窗口显示约 {0:N0} ms" -f ((Get-Date) - $t0).TotalMilliseconds)

    # 右键菜单：弹出（模态）→ 截图 → 取消。菜单出现在光标处，会在屏幕上短暂出现。
    Click-Tray $a $WM_RBUTTONUP
    $menu = $null
    [void](WaitUntil { $m = [Win]::ByClass([uint32]$a.Process.Id, '#32768', $true); if ($m.Count -gt 0) { $script:menu = $m[0]; $true } else { $false } } 5)
    Check ($null -ne $script:menu) '托盘右键：弹出菜单窗口'
    if ($script:menu) {
        Start-Sleep -Milliseconds 400
        $mr = New-Object Win+RECT
        [void][Win]::GetWindowRect($script:menu, [ref]$mr)
        $w = $mr.R - $mr.L; $h = $mr.B - $mr.T
        $bmp = New-Object System.Drawing.Bitmap $w, $h
        $g = [System.Drawing.Graphics]::FromImage($bmp); $dc = $g.GetHdc()
        [void][Win]::PrintWindow($script:menu, $dc, 2)
        $g.ReleaseHdc($dc); $g.Dispose()
        $png = Join-Path $Out '托盘右键菜单.png'
        $bmp.Save($png, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
        "  菜单截图 ${w}x${h} -> $png"
        [void][Win]::SendMessage($a.Tray, $WM_CANCELMODE, [IntPtr]::Zero, [IntPtr]::Zero)
        Check (WaitUntil { [Win]::ByClass([uint32]$a.Process.Id, '#32768', $true).Count -eq 0 } 3) '取消后菜单关闭'
    }
    $a.Process.Refresh()
    Check (-not $a.Process.HasExited) '取消菜单后进程仍在'

    [void][Win]::PostMessage($a.Tray, $WM_COMMAND, [IntPtr]$CMD_EXIT, [IntPtr]::Zero)
    Check ($a.Process.WaitForExit(8000)) '菜单「退出」：进程结束'
    Check (Test-Path (Join-Path $env:TML_SETTINGS_DIR 'settings.json')) '设置文件仍在'
    Check ((Get-Content (Join-Path $env:TML_SETTINGS_DIR 'settings.json') -Raw) -match '"close_to_tray": true') '退出时设置被保存且保留 close_to_tray'

    '=== 默认设置（close_to_tray=false）：关闭窗口即退出，不创建托盘图标 ==='
    $d = Start-Primary $false $false
    Start-Sleep -Seconds 2
    Check ($d.Tray -eq [IntPtr]::Zero) '未开启时不创建托盘窗口'
    [void][Win]::PostMessage($d.Main, $WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero)
    Check ($d.Process.WaitForExit(8000)) 'WM_CLOSE：进程退出'

    foreach ($bg in $false, $true) {
        "=== 隐藏到托盘 $HiddenSeconds 秒的常驻 CPU（后台采样$(if ($bg) { '开启' } else { '关闭' })）==="
        $c = Start-Primary $bg
        Start-Sleep -Seconds 3
        Click-Tray $c $WM_LBUTTONUP
        [void](WaitUntil { -not [Win]::Visible($c.Main) } 3)
        Start-Sleep -Milliseconds 1500
        $c.Process.Refresh(); $cpu0 = $c.Process.TotalProcessorTime.TotalMilliseconds
        Start-Sleep -Seconds $HiddenSeconds
        $c.Process.Refresh(); $cpu1 = $c.Process.TotalProcessorTime.TotalMilliseconds
        ("  CPU 占整机 {0:N3}%（{1:N0} ms CPU 时间 / {2} 秒，{3} 核）" -f (($cpu1 - $cpu0) / ($HiddenSeconds * 1000 * [Environment]::ProcessorCount) * 100), ($cpu1 - $cpu0), $HiddenSeconds, [Environment]::ProcessorCount)
        if (-not $bg) { Check (($cpu1 - $cpu0) -lt 30) '后台采样关闭：隐藏期间 CPU 时间接近 0（< 30 ms）' }
        $t1 = Get-Date
        Click-Tray $c $WM_LBUTTONUP
        Check (WaitUntil { [Win]::Visible($c.Main) } 3) '托盘单击后窗口重新显示'
        Check (WaitUntil { (Log-Text $c) -match '恢复可见' } 3) ("恢复后很快有新的完整采样（{0:N0} ms 内写出「恢复可见」）" -f ((Get-Date) - $t1).TotalMilliseconds)
        $line = (Log-Text $c) -split "`r?`n" | Where-Object { $_ -match '恢复可见' } | Select-Object -Last 1
        "  日志：$line"
        if ($bg) { Check ($line -match '断点=0 ') '后台采样开启：隐藏期间曲线没有断点' } else { Check ($line -notmatch '断点=0 ') '后台采样关闭：隐藏处曲线断开' }
        [void][Win]::PostMessage($c.Tray, $WM_COMMAND, [IntPtr]$CMD_EXIT, [IntPtr]::Zero)
        [void]$c.Process.WaitForExit(8000)
    }
}
finally {
    foreach ($p in $started) { try { if (-not $p.HasExited) { $p.Kill() } } catch {} }
    Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}

if ($failures.Count -gt 0) { "失败 $($failures.Count) 项"; exit 1 }
'全部通过'
