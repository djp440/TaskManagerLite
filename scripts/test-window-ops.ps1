# 自动验收应用窗口操作（#70）：右键菜单“切换到”“最大化”“最小化”“还原”。
# 做法：启动一个自己的记事本，用
#   --menu-action window-minimize|window-restore|window-maximize|window-activate:<pid>[:group]
# （等价于在分组视图的应用行上右键点菜单项；应用行需要 :group；不依赖 SendInput）让应用对该窗口执行操作，
# 再用 Win32（IsIconic / IsZoomed / GetForegroundWindow，P/Invoke）回读窗口状态，并读应用 stderr / 日志核对菜单项状态与结果。
# 覆盖：最小化 / 还原 / 最大化 / 还原的真实状态变化、菜单项按窗口状态置灰（已最小化时“最小化”灰、正常窗口“还原”灰）、
#   对已处于目标状态的项强行触发时被如实拒绝并记录、非应用行（平铺视图）窗口菜单项隐藏且不执行、
#   “切换到”对最小化窗口先还原再请求前台。
# “切换到”的前台切换在无交互桌面（或本程序没有前台权限）时会被系统的前台锁定拒绝，此时按设计退化为闪烁任务栏：
#   脚本把“窗口已还原且已置前台”与“窗口已还原但退化为闪烁任务栏”区分开，只有两者都不成立才算失败，并在输出里写明实际是哪一种。
# 无法自动化（见 docs/checklist/issue-70.md）：对提权应用的置灰与提示、UWP 应用（计算器）、多虚拟桌面、真实右键点击观感。
# 只启动、只结束脚本自己创建的记事本与应用实例；日志写在临时目录。需要交互式桌面。
# 用法：pwsh scripts/test-window-ops.ps1 [-Exe target/release/TaskManagerLite.exe]
param(
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe"
)

$ErrorActionPreference = 'Stop'
$exePath = (Resolve-Path $Exe).Path
$id = Get-Random -Maximum 99999999
$dir = Join-Path ([System.IO.Path]::GetTempPath()) "tml-winops-$id"
New-Item -ItemType Directory -Force $dir | Out-Null
$logDir = Join-Path $dir 'logs'
$env:TML_LOG_DIR = $logDir

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class TmlWin {
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    // 0 正常，1 最大化，2 最小化，-1 窗口不存在。
    public static int State(IntPtr h) {
        if (!IsWindow(h)) return -1;
        if (IsIconic(h)) return 2;
        if (IsZoomed(h)) return 1;
        return 0;
    }
}
'@

$results = @()
function Check($label, [bool]$ok, $detail = '') {
    $script:results += [pscustomobject]@{ Name = $label; Ok = $ok; Detail = $detail }
    "{0} {1} {2}" -f ($(if ($ok) { '通过' } else { '失败' })), $label, $detail
}

function Log-Text {
    $t = ''
    Get-ChildItem $logDir -Filter *.log -ErrorAction SilentlyContinue | ForEach-Object {
        $t += (Get-Content $_.FullName -Raw -Encoding utf8)
    }
    $t
}

$script:errCount = 0
function Run-App($action, $pid_, $extra = ':group') {
    $script:errCount++
    $err = Join-Path $dir ("err-{0}.txt" -f $script:errCount)
    $p = Start-Process $exePath -ArgumentList '--menu-action', "${action}:${pid_}${extra}" -PassThru -RedirectStandardError $err
    [pscustomobject]@{ Proc = $p; Err = $err }
}
function Stderr-Text($app) {
    if (Test-Path $app.Err) {
        $t = Get-Content $app.Err -Raw -Encoding utf8
        if ($t) { return $t }
    }
    ''
}
function Stop-App($app) { if ($app -and $app.Proc -and -not $app.Proc.HasExited) { $app.Proc.Kill(); $app.Proc.WaitForExit() } }
function Wait-For($probe, $seconds) {
    $end = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $end) {
        $v = & $probe
        if ($v) { return $v }
        Start-Sleep -Milliseconds 300
    }
    return $null
}
# 窗口操作日志行数（成功 / 失败 / 闪烁任务栏 / 拒绝），用来等一个钩子实例执行完动作。
function Op-Lines { ([regex]::Matches((Log-Text), '窗口操作 (成功|失败|闪烁任务栏|拒绝)')).Count }

# 在应用里执行一个窗口操作并等它记录结果；返回 stderr 文本。
function Do-Op($action, $pid_, $extra = ':group') {
    $before = Op-Lines
    $app = Run-App $action $pid_ $extra
    $null = Wait-For { if ((Op-Lines) -gt $before) { 1 } } 25
    $err = Stderr-Text $app
    Stop-App $app
    $err
}

$notepad = $null
try {
    $notepad = Start-Process notepad.exe -PassThru
    $np = $notepad.Id
    $h = [IntPtr]::Zero
    $null = Wait-For { $notepad.Refresh(); if ($notepad.MainWindowHandle -ne [IntPtr]::Zero) { 1 } } 15
    $h = $notepad.MainWindowHandle
    if ($h -eq [IntPtr]::Zero) { throw '记事本没有出现窗口' }
    Start-Sleep -Milliseconds 500
    Check '初始状态：记事本窗口是正常大小' ([TmlWin]::State($h) -eq 0)

    # 最小化。
    $err = Do-Op 'window-minimize' $np
    Check '最小化：菜单项显示且可用' ($err -match 'window-minimize 菜单项 visible=True enabled=True') $err.Trim()
    Check '最小化：窗口真的被最小化' ([TmlWin]::State($h) -eq 2)
    Check '最小化：日志记录成功与 PID' ((Log-Text) -match "窗口操作 成功 pid=$np 操作=Minimize")

    # 已最小化时：“最小化”置灰（钩子仍强行触发一次，核对不会出错，状态不变）。
    $err = Do-Op 'window-minimize' $np
    Check '已最小化：“最小化”菜单项置灰且标题写明原因' ($err -match 'enabled=false|enabled=False' -and $err -match '已最小化') $err.Trim()
    Check '已最小化：强行触发后窗口仍是最小化' ([TmlWin]::State($h) -eq 2)

    # 还原。
    $err = Do-Op 'window-restore' $np
    Check '还原：菜单项可用' ($err -match 'window-restore 菜单项 visible=True enabled=True') $err.Trim()
    Check '还原：最小化的窗口恢复为正常大小' ([TmlWin]::State($h) -eq 0)

    # 正常窗口：“还原”置灰；强行触发时窗口状态没有变化，如实报告失败。
    $before = (Log-Text)
    $err = Do-Op 'window-restore' $np
    Check '正常窗口：“还原”置灰且标题写明原因' ($err -match 'enabled=false|enabled=False' -and $err -match '窗口已是正常大小') $err.Trim()
    Check '正常窗口：强行还原被如实拒绝并记录' ((Log-Text) -match "窗口操作 失败 pid=$np 操作=Restore 原因=NotApplied")

    # 最大化。
    $err = Do-Op 'window-maximize' $np
    Check '最大化：菜单项可用' ($err -match 'window-maximize 菜单项 visible=True enabled=True') $err.Trim()
    Check '最大化：窗口真的被最大化' ([TmlWin]::State($h) -eq 1)
    Check '最大化：日志记录成功与 PID' ((Log-Text) -match "窗口操作 成功 pid=$np 操作=Maximize")

    # 已最大化：“最大化”置灰，“还原”可用。
    $err = Do-Op 'window-maximize' $np
    Check '已最大化：“最大化”菜单项置灰且标题写明原因' ($err -match 'enabled=false|enabled=False' -and $err -match '已最大化') $err.Trim()
    $err = Do-Op 'window-restore' $np
    Check '已最大化：“还原”可用并恢复为正常大小' ($err -match 'window-restore 菜单项 visible=True enabled=True' -and [TmlWin]::State($h) -eq 0) $err.Trim()

    # 非应用行（平铺视图，不带 :group）：窗口菜单项隐藏，强行触发也不执行。
    $err = Do-Op 'window-minimize' $np ''
    Check '非应用行：窗口菜单项隐藏' ($err -match 'window-minimize 菜单项 visible=False') $err.Trim()
    Check '非应用行：强行触发被拒绝且窗口未动' ((Log-Text) -match '窗口操作 拒绝 操作=Minimize 原因=所选行不是应用' -and [TmlWin]::State($h) -eq 0)

    # 切换到：先把窗口最小化，再切换；窗口必须被还原，并且要么拿到前台、要么退化为闪烁任务栏。
    [void][TmlWin]::ShowWindow($h, 6)   # SW_MINIMIZE，由脚本自己最小化
    Start-Sleep -Milliseconds 500
    Check '切换到的前提：窗口已被最小化' ([TmlWin]::State($h) -eq 2)
    $flashBefore = ([regex]::Matches((Log-Text), '窗口操作 闪烁任务栏')).Count
    $err = Do-Op 'window-activate' $np
    Check '切换到：菜单项可用' ($err -match 'window-activate 菜单项 visible=True enabled=True') $err.Trim()
    $restored = [TmlWin]::State($h) -ne 2
    Check '切换到：最小化的窗口被还原' $restored
    $text = Log-Text
    $okLog = $text -match "窗口操作 成功 pid=$np 操作=Activate"
    $flashLog = ([regex]::Matches($text, '窗口操作 闪烁任务栏')).Count -gt $flashBefore
    $fg = [TmlWin]::GetForegroundWindow() -eq $h
    if ($okLog) {
        Check '切换到：系统接受了置前台请求' $true "前台窗口就是记事本=$fg"
    }
    elseif ($flashLog) {
        Check '切换到：前台锁定拒绝了置前台，已按设计退化为闪烁任务栏（不算失败）' $true '本次没有前台权限（无交互桌面或前台被锁定）'
    }
    else {
        Check '切换到：既没有成功也没有退化为闪烁任务栏' $false ((Log-Text) -split "`n" | Where-Object { $_ -match '窗口操作' } | Out-String)
    }

    # 日志只含 PID、结果与错误码：不含窗口标题 / 进程名。
    $text = Log-Text
    Check '日志不含窗口标题或进程名' (-not ($text -match '(?i)notepad|记事本|无标题|Untitled'))
}
finally {
    if ($notepad -and -not $notepad.HasExited) { $notepad.Kill(); $notepad.WaitForExit() }
    Start-Sleep -Milliseconds 300
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
}

$failed = @($results | Where-Object { -not $_.Ok })
"共 $($results.Count) 项，失败 $($failed.Count) 项"
if ($failed.Count -gt 0) { exit 1 }
