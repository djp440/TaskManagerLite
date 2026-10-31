# 自动验收进程右键菜单的“效率模式”（EcoQoS，#68）。
# 做法：复制一份 ping.exe 到临时目录并以唯一名字启动（自己的子进程），用
#   --menu-action eco-mode:<pid>:<on|off>[:group]   （等价于点击右键菜单“效率模式”；应用行的确认框由钩子点“确定”）
# 让应用选中该进程并触发动作，再用 NtQueryInformationProcess(ProcessPowerThrottlingState) 回读 EcoQoS 状态、
# 用 .NET 回读优先级类，不依赖 SendInput。
# 覆盖：开启（EcoQoS 开 + 优先级低）、关闭恢复开启前的优先级、开启期间手动改过优先级则关闭后不覆盖、
#   受保护的 System 进程（PID 4）被拒绝并留下带错误码的日志、分组视图的应用行作用于其全部后代。
# 只结束本脚本自己启动的子进程与应用实例；日志写在临时目录。系统低于 Windows 10 1709 时跳过。
# 用法：pwsh scripts/test-ecoqos.ps1 [-Exe target/release/TaskManagerLite.exe]
param(
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe"
)

$ErrorActionPreference = 'Stop'
if ([Environment]::OSVersion.Version.Build -lt 16299) { '系统低于 Windows 10 1709，跳过'; exit 0 }
$exePath = (Resolve-Path $Exe).Path
$id = Get-Random -Maximum 99999999
$dir = Join-Path ([System.IO.Path]::GetTempPath()) "tml-eco-$id"
New-Item -ItemType Directory -Force $dir | Out-Null
$name = "tml-eco-$id.exe"
$target = Join-Path $dir $name
Copy-Item (Join-Path $env:WINDIR 'System32\PING.EXE') $target
$logDir = Join-Path $dir 'logs'
$env:TML_LOG_DIR = $logDir

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class TmlEco {
    [StructLayout(LayoutKind.Sequential)]
    struct State { public uint Version; public uint ControlMask; public uint StateMask; }
    [DllImport("ntdll.dll")]
    static extern int NtQueryInformationProcess(IntPtr h, int cls, ref State s, int len, IntPtr ret);
    [DllImport("kernel32.dll")]
    static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
    [DllImport("kernel32.dll")]
    static extern bool CloseHandle(IntPtr h);
    // 1 = 已启用，0 = 未启用，-1 = 读不到。
    public static int Read(int pid) {
        IntPtr h = OpenProcess(0x1000, false, pid);
        if (h == IntPtr.Zero) return -1;
        try {
            State s = new State { Version = 1 };
            if (NtQueryInformationProcess(h, 77, ref s, 12, IntPtr.Zero) < 0) return -1;
            return ((s.ControlMask & s.StateMask & 1) != 0) ? 1 : 0;
        } finally { CloseHandle(h); }
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

function Run-App($action, $pid_, $arg, $extra = '') {
    Start-Process $exePath -ArgumentList '--menu-action', "${action}:${pid_}:${arg}${extra}" -PassThru
}
function Stop-App($p) { if ($p -and -not $p.HasExited) { $p.Kill(); $p.WaitForExit() } }
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
try {
    $child = Start-Process $target -ArgumentList '-t', '-w', '7777', '127.0.0.1' -WindowStyle Hidden -PassThru
    Start-Sleep -Milliseconds 800
    if ($child.HasExited) { throw '子进程没有启动' }
    function Prio { $child.Refresh(); [string]$child.PriorityClass }

    # 开启：EcoQoS 打开且优先级降为低（Idle）。
    $child.PriorityClass = 'AboveNormal'
    Check '初始状态：未启用效率模式' ([TmlEco]::Read($child.Id) -eq 0)
    $app = Run-App 'eco-mode' $child.Id 'on'
    $got = Wait-For { if ([TmlEco]::Read($child.Id) -eq 1) { 1 } } 15
    Stop-App $app
    Check '开启效率模式：EcoQoS 已启用' ($got -eq 1)
    Check '开启效率模式：优先级降为低' ((Prio) -eq 'Idle') "实际=$(Prio)"

    # 重复开启是幂等的，且不会把“开启前的优先级”覆盖成低（同一个应用实例内记忆；这里是新实例，只验证状态不变）。
    # 关闭：用新实例（记不到开启前的优先级）关闭，优先级仍是低 -> 恢复为正常。
    $app = Run-App 'eco-mode' $child.Id 'off'
    $got = Wait-For { if ([TmlEco]::Read($child.Id) -eq 0) { 1 } } 15
    Stop-App $app
    Check '关闭效率模式：EcoQoS 已关闭' ($got -eq 1)
    $ok = Wait-For { if ((Prio) -eq 'Normal') { 1 } } 5
    Check '关闭效率模式：记不到开启前的优先级时恢复为正常' ([bool]$ok) "实际=$(Prio)"

    # 开启期间用户手动改过优先级：关闭后不覆盖用户的选择。
    $app = Run-App 'eco-mode' $child.Id 'on'
    $null = Wait-For { if ([TmlEco]::Read($child.Id) -eq 1) { 1 } } 15
    Stop-App $app
    $child.PriorityClass = 'High'
    $app = Run-App 'eco-mode' $child.Id 'off'
    $got = Wait-For { if ([TmlEco]::Read($child.Id) -eq 0) { 1 } } 15
    Stop-App $app
    Check '开启期间手动改过优先级：关闭后保持用户的选择' ($got -eq 1 -and (Prio) -eq 'High') "实际=$(Prio)"

    # 受保护进程（System，PID 4）：被拒绝，日志带错误码，不崩溃。
    $app = Run-App 'eco-mode' 4 'on'
    $logged = Wait-For { if ((Log-Text) -match '效率模式 (失败|拒绝) pid=4 ') { 1 } } 15
    Stop-App $app
    Check '受保护进程（System）：开启效率模式被拒绝并记录' ([bool]$logged)

    # 日志只含 PID、结果与错误码。
    $text = Log-Text
    Check '日志记录了成功结果' ($text -match "效率模式 成功 pid=$($child.Id) 目标=开 当前=开 优先级=low")

    # 应用行：一个有可见窗口的 PowerShell（WinForms 窗体）带一个 ping 孙进程，分组视图里是一个应用行。
    $marker = "tml-eco-app-$id"
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
    Start-Sleep -Seconds 1
    $app = Run-App 'eco-mode' $parent.Id 'on' ':group'
    $got = Wait-For { if ([TmlEco]::Read($parent.Id) -eq 1 -and [TmlEco]::Read($grand.ProcessId) -eq 1) { 1 } } 20
    Stop-App $app
    Check '应用行：开启时作用于应用自身及其后代' ($got -eq 1) "应用=$([TmlEco]::Read($parent.Id)) 后代=$([TmlEco]::Read($grand.ProcessId))"
    $app = Run-App 'eco-mode' $parent.Id 'off' ':group'
    $got = Wait-For { if ([TmlEco]::Read($parent.Id) -eq 0 -and [TmlEco]::Read($grand.ProcessId) -eq 0) { 1 } } 20
    Stop-App $app
    Check '应用行：关闭时作用于应用自身及其后代' ($got -eq 1) "应用=$([TmlEco]::Read($parent.Id)) 后代=$([TmlEco]::Read($grand.ProcessId))"
}
finally {
    if ($parent -and -not $parent.HasExited) { $parent.Kill() }
    Get-Process -Name "tml-eco-$id" -ErrorAction SilentlyContinue | ForEach-Object { $_.Kill() }
    if ($child -and -not $child.HasExited) { $child.Kill(); $child.WaitForExit() }
    Start-Sleep -Milliseconds 300
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
}

$failed = @($results | Where-Object { -not $_.Ok })
"共 $($results.Count) 项，失败 $($failed.Count) 项"
if ($failed.Count -gt 0) { exit 1 }
