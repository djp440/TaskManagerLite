# 生成进程右键菜单的验收截图（深/浅色 x 普通进程/受保护进程）。
# 右键菜单是系统原生弹出菜单，应用内截图（take_snapshot）截不到，所以这里：
#   1. 以 --menu-shot 启动应用，应用自行在第一行右键（窗口放在屏幕外，菜单会被系统夹到可见区域，约几秒）；
#   2. 本脚本找到该进程的菜单窗口（类名 #32768），用 PrintWindow 抓成 PNG；
#   3. 结束这个自己启动的应用进程。
# 用法：pwsh scripts/menu-screenshots.ps1 [-Out <目录>] [-Exe target/release/TaskManagerLite.exe]
param(
    [string]$Out = (Join-Path ([System.IO.Path]::GetTempPath()) "tml-menu-shots"),
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
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    // 属于 pid 的可见菜单窗口。
    public static List<IntPtr> Menus(uint pid) {
        var r = new List<IntPtr>();
        EnumWindows((h, l) => {
            uint p; GetWindowThreadProcessId(h, out p);
            var sb = new StringBuilder(64); GetClassName(h, sb, 64);
            if (p == pid && sb.ToString() == "#32768" && IsWindowVisible(h)) r.Add(h);
            return true;
        }, IntPtr.Zero);
        return r;
    }
}
'@

# 声明 DPI 感知，否则高分屏下取到的窗口矩形是缩小后的。
[void][Win]::SetProcessDPIAware()
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$exePath = (Resolve-Path $Exe).Path
$env:TML_LOG_DIR = Join-Path ([System.IO.Path]::GetTempPath()) "tml-menu-log"

$cases = @(
    @{ Name = '右键菜单-普通进程-深色'; Spec = 'explorer:dark' },
    @{ Name = '右键菜单-受保护进程-深色'; Spec = '4:pid:dark' },
    @{ Name = '右键菜单-普通进程-浅色'; Spec = 'explorer:light' },
    @{ Name = '右键菜单-受保护进程-浅色'; Spec = '4:pid:light' }
)
foreach ($c in $cases) {
    $p = Start-Process -FilePath $exePath -ArgumentList '--menu-shot', $c.Spec -PassThru
    try {
        $menu = $null
        for ($i = 0; $i -lt 100 -and -not $menu; $i++) {
            Start-Sleep -Milliseconds 250
            $m = [Win]::Menus([uint32]$p.Id)
            if ($m.Count -gt 0) { $menu = $m[0] }
        }
        if (-not $menu) { throw "$($c.Name)：未出现菜单窗口" }
        Start-Sleep -Milliseconds 600
        $r = New-Object Win+RECT
        [void][Win]::GetWindowRect($menu, [ref]$r)
        $w = $r.R - $r.L; $h = $r.B - $r.T
        $bmp = New-Object System.Drawing.Bitmap $w, $h
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $dc = $g.GetHdc()
        $ok = [Win]::PrintWindow($menu, $dc, 2)
        $g.ReleaseHdc($dc); $g.Dispose()
        $file = Join-Path $Out "$($c.Name).png"
        $bmp.Save($file, [System.Drawing.Imaging.ImageFormat]::Png)
        $bmp.Dispose()
        "$($c.Name)：${w}x${h} PrintWindow=$ok -> $file"
    } finally {
        if (-not $p.HasExited) { $p.Kill() }
        $p.WaitForExit()
    }
}
