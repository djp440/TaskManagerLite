# 运行新任务端到端验收（#69）。启动自己的实例（--allow-multiple，临时 TML_SETTINGS_DIR / TML_LOG_DIR / TML_INSTANCE_ID），
# 用 SendInput 向前台窗口发真实按键，再读应用日志与设置文件核对；结束时只结束自己启动的进程（记事本只清理本脚本新出现的那几个）。
#   1. Ctrl+N 打开对话框（进程页与性能页都行），对话框打开时再按 Ctrl+N 不会打开第二个；回车在输入为空时什么也不做。
#   2. 输入不存在的程序后回车：日志记录失败与错误码 2，对话框保持打开（没有关闭事件），且还能继续输入。
#   3. 输入 notepad 回车：记事本进程出现，对话框关闭，设置文件里的 run_history 记录 notepad。
#   4. 再次 Ctrl+N：日志里历史=1（旧版设置文件没有 run_history 字段也能正常启动）；Esc 关闭对话框。
#   5. “浏览…”：Tab 到按钮按空格，弹出系统文件选择对话框（类名 #32770），Esc 取消后回到运行对话框。
#   6. 在 -Out 目录保存对话框的截图，供目视检查。
# 不覆盖（会弹 UAC 或影响桌面，见 docs/checklist/issue-69.md）：以管理员权限运行、网址、explorer。
# 用法：pwsh scripts/test-run-task.ps1 [-Out <截图目录>] [-Exe target/release/TaskManagerLite.exe]
# 前提：脚本运行期间不要动键盘鼠标和窗口焦点（按键是真实输入，会发给前台窗口）；需要交互式桌面。
param(
    [string]$Out = (Join-Path ([System.IO.Path]::GetTempPath()) 'tml-run-task'),
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe"
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class Win {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [StructLayout(LayoutKind.Sequential)] struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr extra; }
    [StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort vk, scan; public uint flags, time; public IntPtr extra; }
    [StructLayout(LayoutKind.Explicit)] struct InputUnion { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
    [StructLayout(LayoutKind.Sequential)] struct INPUT { public uint type; public InputUnion u; }
    [DllImport("user32.dll")] static extern uint SendInput(uint n, INPUT[] inputs, int size);
    [DllImport("user32.dll")] static extern uint MapVirtualKey(uint code, uint type);
    [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder s, int n);

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
    public static string ForegroundClass() {
        var sb = new StringBuilder(256); GetClassName(GetForegroundWindow(), sb, 256); return sb.ToString();
    }
}
'@
[void][Win]::SetProcessDPIAware()

$exePath = (Resolve-Path $Exe).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$root = Join-Path ([System.IO.Path]::GetTempPath()) "tml-run-task-test-$(Get-Random)"
$settingsDir = Join-Path $root 'settings'; $logDir = Join-Path $root 'logs'
New-Item -ItemType Directory -Force $settingsDir, $logDir | Out-Null
# 旧版设置文件：没有 run_history 字段，应按空历史处理。
Set-Content -Path (Join-Path $settingsDir 'settings.json') -Value '{ "interval_index": 1, "view": "flat" }' -Encoding utf8
$VK_CTRL = [uint16]0x11; $VK_ESC = [uint16]0x1B; $VK_RETURN = [uint16]0x0D; $VK_TAB = [uint16]0x09; $VK_SPACE = [uint16]0x20
$VK_N = [uint16]0x4E; $VK_A = [uint16]0x41
$failures = New-Object System.Collections.Generic.List[string]

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
# 执行 $action，等日志里 $pattern 的匹配数增加。
function Expect-Log([scriptblock]$action, $pattern, $msg, $sec = 5) {
    $before = Log-Count $pattern
    & $action
    Check (WaitUntil { (Log-Count $pattern) -gt $before } $sec) $msg
}
# 执行 $action，等一会儿，日志里 $pattern 的匹配数不应增加。
function Expect-No-Log([scriptblock]$action, $pattern, $msg) {
    $before = Log-Count $pattern
    & $action
    Start-Sleep -Milliseconds 900
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
function Ctrl-Key([uint16]$vk) { Focus-App; [Win]::Chord($VK_CTRL, $vk) }
function Press([uint16]$vk) { Focus-App; [Win]::Chord($vk) }
function Type-Text($text) {
    Focus-App
    foreach ($c in $text.ToUpper().ToCharArray()) { [Win]::Chord([uint16][int]$c); Start-Sleep -Milliseconds 40 }
}
# 回车提交：有输入法（中文输入法即使在英文状态）时，第一下回车可能只是提交输入法里的组合文字，
# 等不到日志就再按一次（再等不到才算失败）。
function Submit-Enter($pattern, $msg) {
    $before = Log-Count $pattern
    Press $VK_RETURN
    if (-not (WaitUntil { (Log-Count $pattern) -gt $before } 1.5)) {
        '  （第一下回车没有触发，补按一次）'
        Press $VK_RETURN
    }
    Check (WaitUntil { (Log-Count $pattern) -gt $before } 5) $msg
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
function Notepad-Ids { @(Get-Process -Name notepad -ErrorAction SilentlyContinue | ForEach-Object { $_.Id }) }

$env:TML_INSTANCE_ID = "runtask$(Get-Random)"
$env:TML_SETTINGS_DIR = $settingsDir
$env:TML_LOG_DIR = $logDir
$notepadBefore = Notepad-Ids
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
    Check (WaitUntil { (Log-Count '首个快照帧') -gt 0 } 10) '应用已出首个快照（旧版设置文件没有 run_history 也正常启动）'
    Focus-App

    '=== 1. Ctrl+N 打开对话框 ==='
    Expect-Log { Ctrl-Key $VK_N } '运行新任务 打开对话框 历史=0' '进程页 Ctrl+N 打开对话框，历史为空'
    Shot '1-运行新任务-空'
    Expect-No-Log { Ctrl-Key $VK_N } '运行新任务 打开对话框' '对话框打开时再按 Ctrl+N 不会打开第二个'
    Expect-No-Log { Press $VK_RETURN } '运行新任务 开始' '输入为空时回车什么也不做'

    '=== 2. 不存在的程序：显示原因，对话框不关闭 ==='
    Type-Text 'zzznosuchprogram'
    Submit-Enter '运行新任务 失败 管理员=false 错误码=2' '回车后启动失败，错误码 2（找不到文件）'
    Check ((Log-Count '运行新任务 成功') -eq 0) '没有记成成功'
    Check ((Log-Count '界面事件 run-task close') -eq 0) '失败后对话框没有关闭'
    Shot '2-运行新任务-错误提示'
    # 对话框仍可操作：退格清空已有输入再改（比 Ctrl+A 在输入法下更稳）。
    Focus-App; 1..24 | ForEach-Object { [Win]::Chord([uint16]0x08) }

    '=== 3. notepad：启动记事本，对话框关闭，写入历史 ==='
    Type-Text 'notepad'
    Submit-Enter '运行新任务 成功 管理员=false' '回车后启动成功'
    Check (WaitUntil { (Log-Count '界面事件 run-task close') -ge 1 } 3) '成功后对话框关闭'
    $newNotepad = @()
    Check (WaitUntil { $script:newNotepad = @(Notepad-Ids | Where-Object { $notepadBefore -notcontains $_ }); $script:newNotepad.Count -ge 1 } 8) '出现了新的记事本进程'
    Check (WaitUntil {
        $f = Join-Path $settingsDir 'settings.json'
        (Test-Path $f) -and (((Get-Content $f -Raw -Encoding utf8 | ConvertFrom-Json).run_history) -contains 'notepad')
    } 5) '设置文件的 run_history 记录了 notepad'

    '=== 4. 再次打开：历史可见 ==='
    Expect-Log { Ctrl-Key $VK_N } '运行新任务 打开对话框 历史=1' '再次 Ctrl+N 打开，历史有 1 条'
    Shot '4-运行新任务-有历史'
    Expect-Log { Press $VK_ESC } '界面事件 run-task close' 'Esc 关闭对话框'

    '=== 5. 性能页也能 Ctrl+N ==='
    Ctrl-Key ([uint16]0x32)
    Expect-Log { Ctrl-Key $VK_N } '运行新任务 打开对话框 历史=1' '性能页 Ctrl+N 打开对话框'
    Press $VK_ESC

    '=== 6. 浏览…（系统文件选择对话框） ==='
    Ctrl-Key ([uint16]0x31)
    Ctrl-Key $VK_N
    Start-Sleep -Milliseconds 400
    Expect-Log { Press $VK_TAB; Press $VK_SPACE } '运行新任务 浏览 打开文件选择对话框' 'Tab 到“浏览…”按下空格，打开文件选择对话框'
    Check (WaitUntil { [Win]::ForegroundClass() -eq '#32770' } 5) '前台窗口是系统通用对话框（#32770）'
    Shot '6-浏览-文件选择对话框'
    Expect-Log { [Win]::Chord($VK_ESC) } '运行新任务 浏览 已取消' 'Esc 取消选择，回到运行对话框'
    Press $VK_ESC

    Check ($app.HasExited -eq $false) '整个过程应用没有崩溃'
}
finally {
    if ($failures.Count -gt 0) {
        '--- 失败时的相关日志 ---'
        (Log-Text) -split "`r?`n" | Where-Object { $_ -match '运行新任务|界面事件|设置' } | Select-Object -Last 60
    }
    if (-not $app.HasExited) { $app.Kill(); [void]$app.WaitForExit(3000) }
    # 只结束本脚本启动的记事本。
    foreach ($id in @(Notepad-Ids | Where-Object { $notepadBefore -notcontains $_ })) {
        Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
    }
    Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}

if ($failures.Count -gt 0) { "失败 $($failures.Count) 项："; $failures | ForEach-Object { "  - $_" }; exit 1 }
"全部通过（截图在 $Out）"
