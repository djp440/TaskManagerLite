# 自动验收进程右键菜单的“挂起”“恢复”与退出前提示（#67）。
# 做法：复制一份 ping.exe 到临时目录并以唯一名字启动（自己的子进程），用
#   --menu-action suspend:<pid>[:group] / resume:<pid>[:group]   （等价于点击右键菜单；确认框由钩子点“确定”）
#   --menu-action suspend-ask:<pid>[:group]                      （只触发，不点确认框，核对“需要确认”路径）
#   --menu-action exit-prompt[-cancel|-twice]:<pid> / exit-noprompt:<pid>   （挂起后模拟退出，核对退出前提示）
# 让应用选中该进程并触发动作，再用 PowerShell 读 Process.Threads[].WaitReason 回读线程是否真的处于 Suspended，
# 并读应用的 stderr / 日志核对菜单项状态与结果，不依赖 SendInput。
# 覆盖：挂起 / 恢复、重复挂起被拒绝且恢复一次即可、未挂起时恢复被拒绝、PID 4 被拒绝、关键进程（explorer.exe）先弹确认且未执行、
#   分组视图应用行作用于其全部后代并先确认、退出前提示（仍然退出 / 取消 / 同一次退出只弹一次 / 没挂起过则直接退出）。
# 只挂起本脚本自己启动的进程，结束前先恢复再结束；只结束本脚本自己启动的应用实例；日志写在临时目录。
# 用法：pwsh scripts/test-suspend.ps1 [-Exe target/release/TaskManagerLite.exe]
param(
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe"
)

$ErrorActionPreference = 'Stop'
$exePath = (Resolve-Path $Exe).Path
$id = Get-Random -Maximum 99999999
$dir = Join-Path ([System.IO.Path]::GetTempPath()) "tml-sus-$id"
New-Item -ItemType Directory -Force $dir | Out-Null
$name = "tml-sus-$id.exe"
$target = Join-Path $dir $name
Copy-Item (Join-Path $env:WINDIR 'System32\PING.EXE') $target
$logDir = Join-Path $dir 'logs'
$env:TML_LOG_DIR = $logDir

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class TmlSus {
    [DllImport("ntdll.dll")]
    static extern int NtResumeProcess(IntPtr h);
    [DllImport("kernel32.dll")]
    static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
    [DllImport("kernel32.dll")]
    static extern bool CloseHandle(IntPtr h);
    // 清理用：把进程恢复到不再挂起（最多 20 次，线程挂起计数不会小于 0）。
    public static void ResumeAll(int pid) {
        IntPtr h = OpenProcess(0x0800, false, pid);
        if (h == IntPtr.Zero) return;
        try { for (int i = 0; i < 20; i++) NtResumeProcess(h); } finally { CloseHandle(h); }
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

# 所有线程都在 Suspended 等待原因：1 = 是，0 = 否，-1 = 进程不存在。
function Suspended-State($procId) {
    $p = Get-Process -Id $procId -ErrorAction SilentlyContinue
    if (-not $p) { return -1 }
    $threads = @($p.Threads)
    if ($threads.Count -eq 0) { return 0 }
    foreach ($t in $threads) {
        if ($t.ThreadState -ne 'Wait' -or $t.WaitReason -ne 'Suspended') { return 0 }
    }
    return 1
}

$script:errCount = 0
function Run-App($action, $pid_, $extra = '') {
    $script:errCount++
    $err = Join-Path $dir ("err-{0}.txt" -f $script:errCount)
    $arg = "${action}:${pid_}${extra}"
    $p = Start-Process $exePath -ArgumentList '--menu-action', $arg -PassThru -RedirectStandardError $err
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

$child = $null
$parent = $null
$grand = $null
$explorer = $null
try {
    $child = Start-Process $target -ArgumentList '-t', '-w', '7777', '127.0.0.1' -WindowStyle Hidden -PassThru
    Start-Sleep -Milliseconds 800
    if ($child.HasExited) { throw '子进程没有启动' }
    $cid = $child.Id

    Check '初始状态：线程未挂起' ((Suspended-State $cid) -eq 0)

    # 挂起。
    $app = Run-App 'suspend' $cid
    $got = Wait-For { if ((Suspended-State $cid) -eq 1) { 1 } } 15
    Check '挂起：所有线程进入 Suspended 等待' ($got -eq 1)
    $logged = Wait-For { if ((Log-Text) -match "挂起 成功 pid=$cid\b") { 1 } } 5
    Check '挂起：日志记录成功与 PID' ([bool]$logged)
    Stop-App $app

    # 重复挂起：新实例从采样里看到“已挂起”，“挂起”置灰且不再执行（避免计数叠加）。
    $app = Run-App 'suspend' $cid
    $logged = Wait-For { if ((Log-Text) -match "挂起 拒绝 pid=$cid\b") { 1 } } 15
    $err = Stderr-Text $app
    Stop-App $app
    Check '重复挂起：被拒绝并记录' ([bool]$logged)
    Check '重复挂起：“挂起”菜单项置灰且标题写明原因' ($err -match 'suspend 菜单项 enabled=false' -and $err -match '已挂起') $err.Trim()

    # 恢复一次就够（若上一步叠加了挂起，这里恢复后仍是挂起）。
    $app = Run-App 'resume' $cid
    $got = Wait-For { if ((Suspended-State $cid) -eq 0) { 1 } } 15
    $err = Stderr-Text $app
    Stop-App $app
    Check '恢复：恢复一次即脱离挂起（没有叠加）' ($got -eq 1)
    Check '恢复：“恢复”菜单项在已挂起时可用' ($err -match 'resume 菜单项 enabled=true') $err.Trim()
    Check '恢复：日志记录成功与 PID' ((Log-Text) -match "恢复 成功 pid=$cid\b")

    # 未挂起时恢复：置灰且不执行。
    $app = Run-App 'resume' $cid
    $logged = Wait-For { if ((Log-Text) -match "恢复 拒绝 pid=$cid\b") { 1 } } 15
    $err = Stderr-Text $app
    Stop-App $app
    Check '未挂起时恢复：被拒绝并记录' ([bool]$logged)
    Check '未挂起时恢复：“恢复”菜单项置灰' ($err -match 'resume 菜单项 enabled=false') $err.Trim()

    # 受保护进程（System，PID 4）：置灰并被拒绝。
    $app = Run-App 'suspend' 4
    $logged = Wait-For { if ((Log-Text) -match '挂起 拒绝 pid=4 ') { 1 } } 15
    $err = Stderr-Text $app
    Stop-App $app
    Check '受保护进程（System）：挂起被拒绝并记录' ([bool]$logged)
    Check '受保护进程（System）：菜单项置灰' ($err -match 'suspend 菜单项 enabled=false') $err.Trim()

    # 关键进程（explorer.exe）：先弹确认，钩子不点确认时不执行。
    $explorer = Get-Process -Name explorer -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($explorer) {
        $eid = $explorer.Id
        $app = Run-App 'suspend-ask' $eid
        $logged = Wait-For { if ((Log-Text) -match "挂起 需要确认 pid=$eid 进程数=1 含关键进程=true") { 1 } } 15
        Start-Sleep -Seconds 2
        Stop-App $app
        Check '关键进程：挂起前需要二次确认' ([bool]$logged)
        Check '关键进程：未确认前没有被挂起' ((Suspended-State $eid) -ne 1)
    } else {
        '没有 explorer.exe，跳过关键进程确认项'
    }

    # 应用行：一个有可见窗口的 PowerShell（WinForms 窗体）带一个 ping 孙进程，分组视图里是一个应用行。
    $marker = "tml-sus-app-$id"
    $script = @"
Add-Type -AssemblyName System.Windows.Forms
`$p = Start-Process '$target' -ArgumentList '-t','-w','7777','127.0.0.1' -WindowStyle Hidden -PassThru
`$f = New-Object System.Windows.Forms.Form
`$f.Text = '$marker'
`$f.Add_FormClosed({ try { `$p.Kill() } catch {} })
[System.Windows.Forms.Application]::Run(`$f)
"@
    $parent = Start-Process pwsh -ArgumentList '-NoProfile', '-Command', $script -PassThru
    $grand = Wait-For {
        Get-CimInstance Win32_Process -Filter "ParentProcessId=$($parent.Id)" -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -eq $name } | Select-Object -First 1
    } 20
    if (-not $grand) { throw '应用行测试的子进程没有启动' }
    $gid = [int]$grand.ProcessId
    $pidApp = $parent.Id
    Start-Sleep -Seconds 1

    $app = Run-App 'suspend-ask' $pidApp ':group'
    $logged = Wait-For { if ((Log-Text) -match "挂起 需要确认 pid=$pidApp 进程数=\d+ ") { 1 } } 20
    Start-Sleep -Seconds 1
    $askErr = Stderr-Text $app
    Stop-App $app
    $askLine = ((Log-Text) -split "`n" | Where-Object { $_ -match "需要确认 pid=$pidApp" } | Select-Object -First 1)
    Check '应用行：作用于多个进程时先确认' ([bool]$logged -and $askLine -match '进程数=([2-9]|\d\d)') "$askLine $askErr"
    Check '应用行：未确认前应用和后代都没有被挂起' ((Suspended-State $gid) -ne 1 -and (Suspended-State $pidApp) -ne 1)

    $app = Run-App 'suspend' $pidApp ':group'
    $got = Wait-For { if ((Suspended-State $gid) -eq 1 -and (Suspended-State $pidApp) -eq 1) { 1 } } 20
    Stop-App $app
    Check '应用行：确认后应用自身及其后代都被挂起' ($got -eq 1) "应用=$(Suspended-State $pidApp) 后代=$(Suspended-State $gid)"
    $app = Run-App 'resume' $pidApp ':group'
    $got = Wait-For { if ((Suspended-State $gid) -eq 0 -and (Suspended-State $pidApp) -eq 0) { 1 } } 20
    Stop-App $app
    Check '应用行：恢复时应用自身及其后代都恢复' ($got -eq 1) "应用=$(Suspended-State $pidApp) 后代=$(Suspended-State $gid)"

    # 退出前提示：挂起后模拟退出。
    # 1) 选“仍然退出”：应用退出，被挂起的进程保持挂起（不自动恢复）。
    $app = Run-App 'exit-prompt' $cid
    $exited = Wait-For { if ($app.Proc.HasExited) { 1 } } 25
    $text = Log-Text
    Stop-App $app
    Check '退出前提示：本程序挂起的进程仍在时弹出提示' ($text -match '退出前提示 弹出 进程数=1')
    Check '退出前提示：选“仍然退出”后应用退出' ([bool]$exited)
    Check '退出前提示：退出后进程仍保持挂起' ((Suspended-State $cid) -eq 1)

    # 2) 提示不处理（等同取消）：应用继续运行；同一次退出流程提示只弹一次（连续两次退出请求）。
    #    新实例先挂起 -> 该进程已是挂起状态，应用拒绝再挂起，所以先恢复，再让新实例自己挂起。
    $app = Run-App 'resume' $cid
    $null = Wait-For { if ((Suspended-State $cid) -eq 0) { 1 } } 15
    Stop-App $app
    $before = ([regex]::Matches((Log-Text), '退出前提示 弹出')).Count
    $app = Run-App 'exit-prompt-twice' $cid
    $null = Wait-For { if ((Stderr-Text $app) -match '退出前提示 confirm_kind=7') { 1 } } 25
    Start-Sleep -Seconds 2
    $alive = -not $app.Proc.HasExited
    $after = ([regex]::Matches((Log-Text), '退出前提示 弹出')).Count
    Stop-App $app
    Check '退出前提示：取消后应用继续运行' $alive
    Check '退出前提示：同一次退出流程只弹一次' (($after - $before) -eq 1) "新增=$($after - $before)"

    # 3) 该实例没有挂起过任何进程（进程是外部挂起的）：直接退出，不提示。
    $before = ([regex]::Matches((Log-Text), '退出前提示 弹出')).Count
    $app = Run-App 'exit-noprompt' $cid
    $exited = Wait-For { if ($app.Proc.HasExited) { 1 } } 25
    $after = ([regex]::Matches((Log-Text), '退出前提示 弹出')).Count
    $text = Log-Text
    Stop-App $app
    Check '退出前提示：没有本程序挂起的进程时直接退出' ([bool]$exited -and $after -eq $before)
    Check '退出前提示：直接退出时记录了跳过原因' ($text -match '退出前提示 跳过')

    # 清理前确认测试进程已恢复。
    $app = Run-App 'resume' $cid
    $null = Wait-For { if ((Suspended-State $cid) -eq 0) { 1 } } 15
    Stop-App $app
    Check '清理：测试进程已恢复' ((Suspended-State $cid) -eq 0)

    # 日志只含 PID、结果与错误码，不含进程名。
    $text = Log-Text
    Check '日志不含进程名等内容' (-not ($text -match [regex]::Escape($name)))
}
finally {
    foreach ($pp in @($child, $parent)) { if ($pp -and -not $pp.HasExited) { [TmlSus]::ResumeAll($pp.Id) } }
    if ($grand) { [TmlSus]::ResumeAll([int]$grand.ProcessId) }
    Get-Process -Name "tml-sus-$id" -ErrorAction SilentlyContinue | ForEach-Object { [TmlSus]::ResumeAll($_.Id); $_.Kill() }
    if ($parent -and -not $parent.HasExited) { $parent.Kill() }
    if ($child -and -not $child.HasExited) { $child.Kill(); $child.WaitForExit() }
    Start-Sleep -Milliseconds 300
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
}

$failed = @($results | Where-Object { -not $_.Ok })
"共 $($results.Count) 项，失败 $($failed.Count) 项"
if ($failed.Count -gt 0) { exit 1 }
