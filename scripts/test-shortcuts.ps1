# 键盘快捷键端到端验收（#71）。启动自己的实例（--allow-multiple，临时 TML_SETTINGS_DIR / TML_LOG_DIR / TML_INSTANCE_ID），
# 用 SendInput 向前台窗口发真实按键，再读应用日志里的「界面事件」「搜索」「重新枚举」等行核对；结束时只结束自己的进程。
#   1. Ctrl+1…6 依次切到 进程 / 性能 / 启动项 / 服务 / 连接 / 设置（页面编号 0/1/3/4/5/2），Ctrl+6 在设置页再按不会回退。
#   2. Ctrl+F 在进程页、启动项页、服务页聚焦各自的搜索框；在性能页按则跳到进程页的搜索框。
#   3. 搜索框里输入的纯数字不会切页；搜索框有焦点时按 Ctrl+数字仍可切页，且数字不会写进搜索框。
#   4. 搜索框里输入后列表被过滤；Esc 清空搜索、恢复全部行并把焦点还给列表（三个页面各验一次）。
#   5. F5 在进程页补采一次，在启动项、服务、设置页重新走进入页面时的刷新入口。
#   6. 迷你窗口里 Ctrl+数字、Ctrl+F、F5 不生效，Esc 仍能回到完整模式。
#   7. 在 -Out 目录保存各页面截图，供目视检查。
# 用法：pwsh scripts/test-shortcuts.ps1 [-Out <截图目录>] [-Exe target/release/TaskManagerLite.exe]
# 前提：脚本运行期间不要动键盘鼠标和窗口焦点（按键是真实输入，会发给前台窗口）；需要交互式桌面。
param(
    [string]$Out = (Join-Path ([System.IO.Path]::GetTempPath()) 'tml-shortcuts'),
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe"
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class Win {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [StructLayout(LayoutKind.Sequential)] struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr extra; }
    [StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort vk, scan; public uint flags, time; public IntPtr extra; }
    [StructLayout(LayoutKind.Explicit)] struct InputUnion { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
    [StructLayout(LayoutKind.Sequential)] struct INPUT { public uint type; public InputUnion u; }
    [DllImport("user32.dll")] static extern uint SendInput(uint n, INPUT[] inputs, int size);
    [DllImport("user32.dll")] static extern uint MapVirtualKey(uint code, uint type);
    [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] static extern void mouse_event(uint flags, uint dx, uint dy, int data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();

    // 同时按下 vks（按顺序），再按相反顺序松开。
    public static void Chord(params ushort[] vks) {
        foreach (var vk in vks) Send(vk, false);
        System.Threading.Thread.Sleep(30);
        for (int i = vks.Length - 1; i >= 0; i--) Send(vks[i], true);
    }
    static void Send(ushort vk, bool up) {
        var inp = new INPUT[1];
        inp[0].type = 1;
        inp[0].u.ki.vk = vk;
        inp[0].u.ki.scan = (ushort)MapVirtualKey(vk, 0);
        inp[0].u.ki.flags = up ? 2u : 0u;
        SendInput(1, inp, Marshal.SizeOf(typeof(INPUT)));
        System.Threading.Thread.Sleep(15);
    }
    // 把窗口带到前台：先轻点一下 Alt 解除前台锁，再 SetForegroundWindow。
    public static bool Foreground(IntPtr h) {
        keybd_event(0x12, 0, 0, UIntPtr.Zero);
        keybd_event(0x12, 0, 2, UIntPtr.Zero);
        ShowWindow(h, 9);
        return SetForegroundWindow(h);
    }
    public static uint ForegroundPid() {
        uint pid; GetWindowThreadProcessId(GetForegroundWindow(), out pid); return pid;
    }
    public static void Wheel(int clicks) { mouse_event(0x0800, 0, 0, clicks * 120, UIntPtr.Zero); }
}
'@
[void][Win]::SetProcessDPIAware()

$exePath = (Resolve-Path $Exe).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$root = Join-Path ([System.IO.Path]::GetTempPath()) "tml-shortcuts-test-$(Get-Random)"
$settingsDir = Join-Path $root 'settings'; $logDir = Join-Path $root 'logs'
New-Item -ItemType Directory -Force $settingsDir, $logDir | Out-Null
$VK_CTRL = [uint16]0x11; $VK_ESC = [uint16]0x1B; $VK_F5 = [uint16]0x74; $VK_F = [uint16]0x46; $VK_M = [uint16]0x4D
$failures = New-Object System.Collections.Generic.List[string]
$mini = $false

function Check($ok, $msg) {
    if ($ok) { "  通过：$msg" } else { "  失败：$msg"; $script:failures.Add($msg) }
}
function WaitUntil([scriptblock]$cond, $sec) {
    $end = (Get-Date).AddSeconds($sec)
    while ((Get-Date) -lt $end) { if (& $cond) { return $true }; Start-Sleep -Milliseconds 100 }
    [bool](& $cond)
}
function Log-Text {
    $f = Get-ChildItem $logDir -Filter *.log -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($f) { Get-Content $f.FullName -Raw -Encoding utf8 } else { '' }
}
function Log-Count($pattern) { ([regex]::Matches((Log-Text), $pattern)).Count }
# 最后一次匹配的第 1 个捕获组（没有匹配返回 $null）。
function Log-Last($pattern) {
    $m = [regex]::Matches((Log-Text), $pattern)
    if ($m.Count -gt 0) { $m[$m.Count - 1].Groups[1].Value } else { $null }
}
# 执行 $action，等日志里 $pattern 的匹配数增加。
function Expect-Log([scriptblock]$action, $pattern, $msg, $sec = 4) {
    $before = Log-Count $pattern
    & $action
    Check (WaitUntil { (Log-Count $pattern) -gt $before } $sec) $msg
}
# 执行 $action，等一会儿，日志里 $pattern 的匹配数不应增加。
function Expect-No-Log([scriptblock]$action, $pattern, $msg) {
    $before = Log-Count $pattern
    & $action
    Start-Sleep -Milliseconds 800
    Check ((Log-Count $pattern) -eq $before) $msg
}
function Focus-App {
    for ($i = 0; $i -lt 10; $i++) {
        [void][Win]::Foreground($script:main)
        Start-Sleep -Milliseconds 150
        if ([Win]::ForegroundPid() -eq [uint32]$script:app.Id) { return }
    }
    throw '无法把应用窗口带到前台（需要交互式桌面，且运行期间不要切换窗口）'
}
# 迷你窗口里不能再把主窗口带到前台（会把它还原），所以由 $script:mini 切换为只检查前台进程。
function Focus-Keys {
    if ($script:mini) {
        if ([Win]::ForegroundPid() -ne [uint32]$script:app.Id) { throw '迷你窗口不在前台，无法验证' }
    } else { Focus-App }
}
function Ctrl-Key([uint16]$vk) { Focus-Keys; [Win]::Chord($VK_CTRL, $vk) }
function Press([uint16]$vk) { Focus-Keys; [Win]::Chord($vk) }
function Type-Text($text) {
    Focus-Keys
    foreach ($c in $text.ToUpper().ToCharArray()) { [Win]::Chord([uint16][int]$c); Start-Sleep -Milliseconds 40 }
}
function Shot($name) {
    Start-Sleep -Milliseconds 500
    $r = New-Object Win+RECT
    [void][Win]::GetWindowRect($script:main, [ref]$r)
    $w = $r.R - $r.L; $h = $r.B - $r.T
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.L, $r.T, 0, 0, (New-Object System.Drawing.Size $w, $h))
    $g.Dispose()
    $file = Join-Path $Out "$name.png"
    $bmp.Save($file, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
    "  截图 ${w}x${h} -> $file"
}

$env:TML_INSTANCE_ID = "shortcuts$(Get-Random)"
$env:TML_SETTINGS_DIR = $settingsDir
$env:TML_LOG_DIR = $logDir
$app = Start-Process -FilePath $exePath -ArgumentList '--allow-multiple' -PassThru
try {
    $main = [IntPtr]::Zero
    for ($i = 0; $i -lt 100 -and $main -eq [IntPtr]::Zero; $i++) {
        Start-Sleep -Milliseconds 100
        $app.Refresh()
        if ($app.HasExited) { throw "实例提前退出，退出码 $($app.ExitCode)" }
        if ($app.MainWindowHandle -ne [IntPtr]::Zero -and [Win]::IsWindowVisible($app.MainWindowHandle)) { $main = $app.MainWindowHandle }
    }
    if ($main -eq [IntPtr]::Zero) { throw '等不到主窗口' }
    Check (WaitUntil { (Log-Count '首个快照帧') -gt 0 } 10) '应用已出首个快照'
    Focus-App

    '=== 1. Ctrl+1…6 切页 ==='
    Expect-Log { Ctrl-Key ([uint16]0x33) } '界面事件 page=3\b' 'Ctrl+3 切到启动项页（页面 3）'
    Expect-Log { Ctrl-Key ([uint16]0x34) } '界面事件 page=4\b' 'Ctrl+4 切到服务页（页面 4）'
    Expect-Log { Ctrl-Key ([uint16]0x35) } '界面事件 page=5\b' 'Ctrl+5 切到连接页（页面 5）'
    Expect-Log { Ctrl-Key ([uint16]0x36) } '界面事件 page=2\b' 'Ctrl+6 切到设置页（页面 2）'
    Expect-No-Log { Ctrl-Key ([uint16]0x36) } '界面事件 page=' '已在设置页时再按 Ctrl+6 不再切换（不回退）'
    Expect-Log { Ctrl-Key ([uint16]0x32) } '界面事件 page=1\b' 'Ctrl+2 切到性能页（页面 1）'
    Shot '1-性能页'
    Expect-Log { Ctrl-Key ([uint16]0x31) } '界面事件 page=0\b' 'Ctrl+1 切到进程页（页面 0）'
    # 从进程页进入设置页，再用 Ctrl+1 回去：与侧边栏一致（页面编号对应关系已由上面各项覆盖）。

    '=== 2. 进程页：Ctrl+F、输入、数字不切页、Esc ==='
    Expect-Log { Ctrl-Key $VK_F } '界面事件 search-focus page=0 focused=1' 'Ctrl+F 聚焦进程页搜索框'
    Expect-No-Log { Type-Text '34' } '界面事件 page=' '搜索框里输入纯数字 3、4 不切页'
    Check ((Log-Last '进程页 搜索="([^"]*)" 显示') -eq '34') '数字进了搜索框（过滤文字为 34）'
    Expect-Log { Press $VK_ESC } '进程页 搜索="" 显示' 'Esc 清空搜索并刷新列表'
    Check ((Log-Count '界面事件 search-focus page=0 focused=0') -ge 1) 'Esc 之后搜索框失去焦点（焦点回到列表）'
    $all = [int](Log-Last '进程页 搜索="" 显示 (\d+) 行')
    Expect-Log { Ctrl-Key $VK_F; Type-Text 'svchost' } '进程页 搜索="svchost" 显示' '输入 svchost 过滤进程列表'
    $some = [int](Log-Last '进程页 搜索="svchost" 显示 (\d+) 行')
    Check ($some -ge 1 -and $some -lt $all) "过滤后行数 $some 少于全部 $all 且至少一行"
    Shot '2-进程页-搜索svchost'
    Expect-Log { Ctrl-Key ([uint16]0x33) } '界面事件 page=3\b' '搜索框有焦点时 Ctrl+3 仍可切页'
    Check ((Log-Count '搜索="svchost3"') -eq 0) 'Ctrl+3 没有把 3 写进搜索框'
    Ctrl-Key ([uint16]0x31)
    Check (WaitUntil { (Log-Last '界面事件 page=(\d)') -eq '0' } 3) '回到进程页'
    Ctrl-Key $VK_F; Press $VK_ESC   # 清掉进程页残留的 svchost 过滤
    Check ((Log-Last '进程页 搜索="([^"]*)" 显示') -eq '') '进程页搜索已清空'

    '=== 3. 性能页按 Ctrl+F 跳到进程页搜索框 ==='
    Ctrl-Key ([uint16]0x32)
    Check (WaitUntil { (Log-Last '界面事件 page=(\d)') -eq '1' } 3) '在性能页'
    Expect-Log { Ctrl-Key $VK_F } '界面事件 search-focus page=0 focused=1' '性能页 Ctrl+F：聚焦进程页搜索框'
    Check ((Log-Last '界面事件 page=(\d)') -eq '0') 'Ctrl+F 之后页面是进程页'
    Press $VK_ESC

    '=== 4. 启动项页 ==='
    Ctrl-Key ([uint16]0x33)
    Check (WaitUntil { (Log-Last '界面事件 page=(\d)') -eq '3' } 3) '在启动项页'
    Start-Sleep -Seconds 3   # 等后台枚举完成
    Expect-Log { Ctrl-Key $VK_F } '界面事件 search-focus page=3 focused=1' 'Ctrl+F 聚焦启动项页搜索框'
    Expect-No-Log { Type-Text '12' } '界面事件 page=' '启动项搜索框里输入数字不切页'
    Press $VK_ESC
    Expect-Log { Ctrl-Key $VK_F; Type-Text 'zzzzzz' } '启动项页 搜索="zzzzzz" 显示 0/' '输入不存在的名字：启动项页被过滤为 0 行'
    $total = [int](Log-Last '启动项页 搜索="zzzzzz" 显示 0/(\d+)')
    Shot '4-启动项页-搜索无结果'
    Expect-Log { Press $VK_ESC } '启动项页 搜索="" 显示' 'Esc 清空启动项搜索'
    if ($total -gt 0) {
        Check ((Log-Last '启动项页 搜索="" 显示 (\d+)/') -eq "$total") "清空后恢复全部 $total 个启动项"
        $run = Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -ErrorAction SilentlyContinue
        $name = if ($run) { ($run.PSObject.Properties | Where-Object { $_.Name -notlike 'PS*' } | Select-Object -First 1).Name } else { $null }
        if ($name -and $name -match '^[A-Za-z0-9]+$') {
            Expect-Log { Ctrl-Key $VK_F; Type-Text $name } "(?i)启动项页 搜索=`"$name`" 显示 [1-9]" "输入已有启动项 $name：至少匹配一行"
            Shot '4-启动项页-搜索命中'
            Press $VK_ESC
        } else { '  跳过：HKCU Run 里没有纯字母数字名称的启动项，不做命中检查' }
    } else { '  跳过：本机没有启动项，不做清空后恢复与命中检查' }

    '=== 5. 服务页 ==='
    Ctrl-Key ([uint16]0x34)
    Check (WaitUntil { (Log-Last '界面事件 page=(\d)') -eq '4' } 3) '在服务页'
    Check (WaitUntil { (Log-Count '服务页 枚举 \d+ 个服务') -ge 1 } 15) '服务列表已枚举'
    Expect-Log { Ctrl-Key $VK_F } '界面事件 search-focus page=4 focused=1' 'Ctrl+F 聚焦服务页搜索框'
    Expect-No-Log { Type-Text '45' } '界面事件 page=' '服务搜索框里输入数字不切页'
    Press $VK_ESC
    Expect-Log { Ctrl-Key $VK_F; Type-Text 'rpcss' } '服务页 搜索="rpcss" 显示' '输入 rpcss 过滤服务'
    $m = [regex]::Matches((Log-Text), '服务页 搜索="rpcss" 显示 (\d+)/(\d+)'); $last = $m[$m.Count - 1]
    $shown = [int]$last.Groups[1].Value; $svcTotal = [int]$last.Groups[2].Value
    Check ($shown -ge 1 -and $shown -lt $svcTotal) "过滤后 $shown 个服务（共 $svcTotal），至少含 RpcSs"
    Shot '5-服务页-搜索rpcss'
    Expect-Log { Press $VK_ESC } '服务页 搜索="" 显示' 'Esc 清空服务搜索'
    Check ((Log-Last '服务页 搜索="" 显示 (\d+)/') -eq "$svcTotal") "清空后恢复全部 $svcTotal 个服务"

    '=== 6. F5 ==='
    Expect-Log { Press $VK_F5 } '服务页 重新枚举' '服务页 F5：重新枚举（与进入页面同一入口）'
    Ctrl-Key ([uint16]0x33)
    Check (WaitUntil { (Log-Last '界面事件 page=(\d)') -eq '3' } 3) '在启动项页'
    Expect-Log { Press $VK_F5 } '启动项页 重新枚举' '启动项页 F5：重新枚举（与进入页面同一入口）'
    Ctrl-Key ([uint16]0x31)
    Check (WaitUntil { (Log-Last '界面事件 page=(\d)') -eq '0' } 3) '在进程页'
    Expect-Log { Press $VK_F5 } 'F5 请求立即补采' '进程页 F5：请求立即补采一次'
    Ctrl-Key ([uint16]0x32)
    Check (WaitUntil { (Log-Last '界面事件 page=(\d)') -eq '1' } 3) '在性能页'
    Expect-Log { Press $VK_F5 } 'F5 请求立即补采' '性能页 F5：请求立即补采一次'
    # 搜索框里按 F5 也生效（文本框不处理 F5）。
    Ctrl-Key ([uint16]0x31)
    Ctrl-Key $VK_F
    Expect-Log { Press $VK_F5 } 'F5 请求立即补采' '搜索框有焦点时 F5 也生效'
    Press $VK_ESC

    '=== 7. 设置页的快捷键说明 ==='
    Ctrl-Key ([uint16]0x36)
    Check (WaitUntil { (Log-Last '界面事件 page=(\d)') -eq '2' } 3) '在设置页'
    Shot '7-设置页-顶部'
    $r = New-Object Win+RECT; [void][Win]::GetWindowRect($main, [ref]$r)
    [void][Win]::SetCursorPos([int](($r.L + $r.R) / 2), [int](($r.T + $r.B) / 2))
    [Win]::Wheel(-30); Start-Sleep -Milliseconds 600
    Shot '7-设置页-快捷键'
    Press $VK_F5   # 设置页 F5：重新读取注册表状态，不应崩溃
    Start-Sleep -Milliseconds 500
    Check ((Log-Count 'refresh page=2') -ge 1) '设置页 F5 被处理'
    Ctrl-Key ([uint16]0x31)

    '=== 8. 迷你窗口：新快捷键不生效，Esc 仍回到完整模式 ==='
    Expect-Log { Ctrl-Key $VK_M } '进入迷你模式' 'Ctrl+M 进入迷你模式'
    $mini = $true
    Start-Sleep -Milliseconds 800
    $pat = '界面事件 (page=|refresh|search-focus)'
    Expect-No-Log { Ctrl-Key ([uint16]0x33) } $pat '迷你窗口：Ctrl+3 不切页'
    Expect-No-Log { Ctrl-Key $VK_F } $pat '迷你窗口：Ctrl+F 不聚焦搜索框'
    Expect-No-Log { Press $VK_F5 } $pat '迷你窗口：F5 不触发刷新入口'
    Expect-Log { Press $VK_ESC } '回到完整模式' '迷你窗口：Esc 回到完整模式（说明按键确实发给了迷你窗口）'

    Check ($app.HasExited -eq $false) '整个过程应用没有崩溃'
}
finally {
    if ($failures.Count -gt 0) {
        '--- 失败时的相关日志 ---'
        (Log-Text) -split "`r?`n" | Where-Object { $_ -match '界面事件|搜索|重新枚举|F5|迷你|回到完整' } | Select-Object -Last 80
    }
    if (-not $app.HasExited) { $app.Kill(); [void]$app.WaitForExit(3000) }
    Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}

if ($failures.Count -gt 0) { "失败 $($failures.Count) 项："; $failures | ForEach-Object { "  - $_" }; exit 1 }
"全部通过（截图在 $Out）"
