# 自动验收进程右键菜单的“设置优先级”“设置亲和性”（#66）。
# 做法：复制一份 ping.exe 到临时目录并以唯一名字启动（自己的子进程），用
#   --menu-action set-priority:<pid>:<优先级键>   （等价于点击“设置优先级”子菜单项）
#   --menu-action set-affinity:<pid>:<十六进制掩码> （打开亲和性对话框、按掩码勾选、点“确定”）
# 让应用选中该进程并触发动作，再用 .NET 回读子进程的优先级类 / 亲和性核对，不依赖 SendInput。
# 另外核对：选“实时”会先弹确认框（钩子不点确认，所以优先级不变）；受保护的 System 进程（PID 4）被拒绝并留下
# 带错误码的日志；空掩码被拒绝；曾经失败不影响后续成功。日志只记 PID、结果与错误码。
# 只结束本脚本自己启动的子进程与应用实例；日志写在临时目录。
# 用法：pwsh scripts/test-process-priority.ps1 [-Exe target/release/TaskManagerLite.exe]
param(
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe"
)

$ErrorActionPreference = 'Stop'
$exePath = (Resolve-Path $Exe).Path
$id = Get-Random -Maximum 99999999
$dir = Join-Path ([System.IO.Path]::GetTempPath()) "tml-prio-$id"
New-Item -ItemType Directory -Force $dir | Out-Null
$name = "tml-prio-$id.exe"
$target = Join-Path $dir $name
Copy-Item (Join-Path $env:WINDIR 'System32\PING.EXE') $target
$logDir = Join-Path $dir 'logs'
$env:TML_LOG_DIR = $logDir

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

$child = $null
try {
    $child = Start-Process $target -ArgumentList '-t', '-w', '7777', '127.0.0.1' -WindowStyle Hidden -PassThru
    Start-Sleep -Milliseconds 800
    if ($child.HasExited) { throw '子进程没有启动' }

    function Run-App($action, $pid_, $arg) {
        Start-Process $exePath -ArgumentList '--menu-action', "${action}:${pid_}:${arg}" -PassThru
    }
    function Stop-App($p) { if ($p -and -not $p.HasExited) { $p.Kill(); $p.WaitForExit() } }
    function Wait-For([scriptblock]$probe, [int]$seconds = 20) {
        $end = (Get-Date).AddSeconds($seconds)
        while ((Get-Date) -lt $end) {
            $v = & $probe
            if ($v) { return $v }
            Start-Sleep -Milliseconds 300
        }
        return $null
    }
    function Prio { $child.Refresh(); [string]$child.PriorityClass }
    function Mask { $child.Refresh(); [int64]$child.ProcessorAffinity }

    # 逐个优先级（“实时”单独验证）。
    $levels = [ordered]@{
        'high' = 'High'; 'above-normal' = 'AboveNormal'; 'below-normal' = 'BelowNormal'
        'low' = 'Idle'; 'normal' = 'Normal'
    }
    foreach ($k in $levels.Keys) {
        # 先改到与目标不同的值，保证确实发生了变化。
        $child.PriorityClass = if ($k -eq 'normal') { 'High' } else { 'Normal' }
        $app = Run-App 'set-priority' $child.Id $k
        $got = Wait-For { if ((Prio) -eq $levels[$k]) { Prio } } 15
        Stop-App $app
        Check "设置优先级：$k" ($got -eq $levels[$k]) "期望=$($levels[$k]) 实际=$(Prio)"
    }

    # 实时：只弹确认框，钩子不点确认，优先级必须保持不变。
    $child.PriorityClass = 'BelowNormal'
    $app = Run-App 'set-priority' $child.Id 'realtime'
    Start-Sleep -Seconds 6
    Stop-App $app
    Check '设置优先级：实时需要先确认（未确认不生效）' ((Prio) -eq 'BelowNormal') "实际=$(Prio)"

    # 亲和性：先改成全部处理器，再用钩子缩到 CPU0，再放宽到 CPU0+1。
    $cpus = [Environment]::ProcessorCount
    if ($cpus -ge 2) {
        $all = [int64]([math]::Pow(2, [math]::Min($cpus, 62)) - 1)
        $child.ProcessorAffinity = [IntPtr]$all
        $app = Run-App 'set-affinity' $child.Id '1'
        $got = Wait-For { if ((Mask) -eq 1) { 1 } } 15
        Stop-App $app
        Check '设置亲和性：只留 CPU0' ($got -eq 1) "掩码=0x$('{0:x}' -f (Mask))"

        $app = Run-App 'set-affinity' $child.Id '3'
        $got = Wait-For { if ((Mask) -eq 3) { 3 } } 15
        Stop-App $app
        Check '设置亲和性：CPU0+CPU1' ($got -eq 3) "掩码=0x$('{0:x}' -f (Mask))"
    } else {
        "跳过亲和性用例：只有 $cpus 个逻辑处理器"
    }

    # 空掩码被拒绝，亲和性保持不变。
    $before = Mask
    $app = Run-App 'set-affinity' $child.Id '0'
    $logged = Wait-For { if ((Log-Text) -match "设置亲和性 失败 pid=$($child.Id) 错误码=") { 1 } } 15
    Stop-App $app
    Check '设置亲和性：空掩码被拒绝且不改变现状' ($logged -and (Mask) -eq $before) "掩码=0x$('{0:x}' -f (Mask))"

    # 受保护进程（System，PID 4）：被拒绝，日志带错误码。
    $app = Run-App 'set-priority' 4 'low'
    $logged = Wait-For { if ((Log-Text) -match '设置优先级 失败 pid=4 请求=low 错误码=') { 1 } } 15
    Stop-App $app
    Check '受保护进程（System）：设置优先级被拒绝并记录错误码' ([bool]$logged)

    # 日志里记录了成功的结果。
    $text = Log-Text
    Check '日志记录了成功结果' ($text -match "设置优先级 成功 pid=$($child.Id) 请求=low 实际=low")
}
finally {
    Get-Process -Name "tml-prio-$id" -ErrorAction SilentlyContinue | ForEach-Object { $_.Kill() }
    if ($child -and -not $child.HasExited) { $child.Kill(); $child.WaitForExit() }
    Start-Sleep -Milliseconds 300
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
}

$failed = @($results | Where-Object { -not $_.Ok })
"共 $($results.Count) 项，失败 $($failed.Count) 项"
if ($failed.Count -gt 0) { exit 1 }
