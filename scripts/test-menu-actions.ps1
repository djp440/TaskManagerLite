# 自动验收进程右键菜单的四个动作（打开文件位置、属性、复制路径、复制命令行）。
# 做法：复制一份 ping.exe 到临时目录并以唯一名字启动（自己的子进程），用 `--menu-action <动作>:<pid>`
# 让应用选中该进程并触发菜单动作（等价于点击菜单项，免去操作原生菜单），然后检查结果：
#   - 打开文件位置：Shell.Application.Windows() 中出现 LocationURL 为临时目录的资源管理器窗口，且选中了该 exe；
#   - 属性：出现标题含该 exe 名的属性对话框（类名 #32770）；
#   - 复制路径 / 复制命令行：读剪贴板核对。
# 只关闭本脚本自己打开的窗口和启动的进程；剪贴板里原有的文本会在结束时还原。
# “在线搜索”不会真的打开浏览器，其网址构造与编码由 tml-core 的单元测试覆盖。
# 用法：pwsh scripts/test-menu-actions.ps1 [-Exe target/release/TaskManagerLite.exe]
param(
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe"
)

$ErrorActionPreference = 'Stop'
Add-Type @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class W {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    // 标题含 title 的可见对话框（类名 #32770）。
    public static List<IntPtr> Dialogs(string title) {
        var r = new List<IntPtr>();
        EnumWindows((h, l) => {
            var c = new StringBuilder(64); GetClassName(h, c, 64);
            var t = new StringBuilder(512); GetWindowText(h, t, 512);
            if (c.ToString() == "#32770" && IsWindowVisible(h) && t.ToString().Contains(title)) r.Add(h);
            return true;
        }, IntPtr.Zero);
        return r;
    }
}
'@

$exePath = (Resolve-Path $Exe).Path
$id = Get-Random -Maximum 99999999
$dir = Join-Path ([System.IO.Path]::GetTempPath()) "tml-menu-$id"
New-Item -ItemType Directory -Force $dir | Out-Null
$name = "tml-menu-$id.exe"
$target = Join-Path $dir $name
Copy-Item (Join-Path $env:WINDIR 'System32\PING.EXE') $target
$env:TML_LOG_DIR = Join-Path ([System.IO.Path]::GetTempPath()) "tml-menu-log"

$results = @()
function Check($label, [bool]$ok, $detail = '') {
    $script:results += [pscustomobject]@{ Name = $label; Ok = $ok; Detail = $detail }
    "{0} {1} {2}" -f ($(if ($ok) { '通过' } else { '失败' })), $label, $detail
}

# 剪贴板原有文本，结束时还原。
$savedClip = try { Get-Clipboard -Raw -ErrorAction Stop } catch { $null }
$child = $null
try {
    $child = Start-Process $target -ArgumentList '-t', '-w', '7777', '127.0.0.1' -WindowStyle Hidden -PassThru
    Start-Sleep -Milliseconds 800
    if ($child.HasExited) { throw '子进程没有启动' }

    function Start-App($action) {
        Start-Process $exePath -ArgumentList '--menu-action', "${action}:$($child.Id)" -PassThru
    }
    function Stop-App($p) { if ($p -and -not $p.HasExited) { $p.Kill(); $p.WaitForExit() } }
    # 等待 $probe 返回非空，最多 $seconds 秒。
    function Wait-For([scriptblock]$probe, [int]$seconds = 20) {
        $end = (Get-Date).AddSeconds($seconds)
        while ((Get-Date) -lt $end) {
            $v = & $probe
            if ($v) { return $v }
            Start-Sleep -Milliseconds 300
        }
        return $null
    }

    # 复制路径
    Set-Clipboard -Value 'TML-SENTINEL'
    $app = Start-App 'copy-path'
    $got = Wait-For { $c = Get-Clipboard -Raw; if ($c -and $c -ne 'TML-SENTINEL') { $c } }
    Stop-App $app
    Check '复制路径' ($got -and $got.Trim() -ieq $target) "剪贴板=$got"

    # 复制命令行
    Set-Clipboard -Value 'TML-SENTINEL'
    $app = Start-App 'copy-command-line'
    $got = Wait-For { $c = Get-Clipboard -Raw; if ($c -and $c -ne 'TML-SENTINEL') { $c } }
    Stop-App $app
    $line = if ($got) { $got.Trim() } else { '' }
    Check '复制命令行' ($line.Replace('"', '') -ieq "$target -t -w 7777 127.0.0.1") "剪贴板=$line"

    # 打开文件位置：资源管理器窗口定位到临时目录，并选中该 exe。
    $shell = New-Object -ComObject Shell.Application
    $expected = ([uri]$dir).AbsoluteUri.TrimEnd('/')
    $find = { $shell.Windows() | Where-Object { $_.LocationURL -and $_.LocationURL.TrimEnd('/') -ieq $expected } | Select-Object -First 1 }
    $app = Start-App 'open-location'
    $win = Wait-For $find
    $selected = $null
    if ($win) {
        Start-Sleep -Milliseconds 800
        $selected = try { $win.Document.FocusedItem.Path } catch { $null }
    }
    Check '打开文件位置：弹出资源管理器窗口' ([bool]$win) "LocationURL=$($win.LocationURL)"
    Check '打开文件位置：选中了该文件' ($selected -ieq $target) "选中=$selected"
    # 只关闭这个窗口（位置是本脚本的唯一临时目录）。
    if ($win) { try { $win.Quit() } catch {} }
    Stop-App $app

    # 属性：标题含唯一文件名的属性对话框。
    $app = Start-App 'properties'
    $dlg = Wait-For { $d = [W]::Dialogs($name); if ($d.Count -gt 0) { $d[0] } }
    Check '属性：弹出属性对话框' ([bool]$dlg) "窗口=$dlg"
    if ($dlg) { [void][W]::PostMessage($dlg, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) }  # WM_CLOSE
    Start-Sleep -Milliseconds 500
    Stop-App $app
}
finally {
    if ($child -and -not $child.HasExited) { $child.Kill(); $child.WaitForExit() }
    if ($null -ne $savedClip) { Set-Clipboard -Value $savedClip } else { Set-Clipboard -Value $null }
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
}

$failed = @($results | Where-Object { -not $_.Ok })
"共 $($results.Count) 项，失败 $($failed.Count) 项"
if ($failed.Count -gt 0) { exit 1 }
