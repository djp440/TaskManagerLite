# 服务页“启动类型”子菜单（#72）端到端验收：用 `--service-action <动作>:<服务名>[:<类型>]` 启动自己的实例
# （开发模式，不读写设置、不受单实例限制，不弹 UAC），首次列出服务后由应用执行动作并写日志；脚本读日志核对。
#   普通权限：
#     1. menu:RpcSs —— 菜单打勾“自动”、可修改；
#     2. menu:<受保护服务> —— 菜单置灰并注明“受系统保护”（系统里没有受保护服务时跳过）；
#     3. set-start:RpcSs:manual —— 普通权限下被拒绝（错误码 5），进程不崩溃，`sc qc` 读回仍是 AUTO_START。
#   已提权（可选）：
#     4. 创建临时服务 tml-test-svc（start= demand），依次改成 disabled、auto-delayed、auto、manual，
#        每次用 `sc qc` 核对，最后 `sc delete`。未提权时跳过并说明。
# 用法：pwsh scripts/test-service-start-type.ps1 [-Exe target/release/TaskManagerLite.exe]
param(
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe"
)

$ErrorActionPreference = 'Stop'
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$exePath = (Resolve-Path $Exe).Path
$results = @()
function Check($label, [bool]$ok, $detail = '') {
    $script:results += [pscustomobject]@{ Name = $label; Ok = $ok; Detail = $detail }
    "{0} {1} {2}" -f ($(if ($ok) { '通过' } else { '失败' })), $label, $detail
}

# 启动一个实例执行动作，等日志出现 $pattern 后结束它，返回日志全文与进程是否仍在运行。
function Run-Action($spec, $pattern, [int]$seconds = 30) {
    $dir = Join-Path ([System.IO.Path]::GetTempPath()) "tml-svc-start-$(Get-Random)"
    New-Item -ItemType Directory -Force $dir | Out-Null
    $env:TML_LOG_DIR = $dir
    $p = Start-Process $exePath -ArgumentList '--service-action', $spec -PassThru
    $text = ''
    try {
        $end = (Get-Date).AddSeconds($seconds)
        while ((Get-Date) -lt $end) {
            Start-Sleep -Milliseconds 300
            $log = Get-ChildItem $dir -Filter *.log | Select-Object -First 1
            if ($log) { $text = Get-Content $log.FullName -Raw -Encoding utf8 }
            if ($text -match $pattern -or $p.HasExited) { break }
        }
        Start-Sleep -Milliseconds 300
        $alive = -not $p.HasExited
    } finally {
        if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
        Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Item Env:TML_LOG_DIR -ErrorAction SilentlyContinue
    }
    [pscustomobject]@{ Text = $text; Alive = $alive }
}

function Line($text, $pattern) {
    $text -split "`r?`n" | Where-Object { $_ -match $pattern } | Select-Object -First 1
}

# sc qc 的 START_TYPE 行（如 “2   AUTO_START  (DELAYED)”）。
function Start-TypeOf($name) {
    $line = (sc.exe qc $name | Select-String 'START_TYPE') -replace '^\s*START_TYPE\s*:\s*', ''
    "$line".Trim()
}

# 1：普通服务的菜单状态
$r = Run-Action 'menu:RpcSs' '服务菜单 '
$line = Line $r.Text '服务菜单 '
Check 'RpcSs 菜单打勾“自动”且可修改' ($line -match '当前=0 可修改=true 标题=启动类型$') "$line"

# 2：受保护服务置灰（注册表 LaunchProtected 非 0 且出现在服务列表里的第一个）
$protected = Get-ChildItem 'HKLM:\SYSTEM\CurrentControlSet\Services' -ErrorAction SilentlyContinue |
    Where-Object { ($_.GetValue('LaunchProtected') -as [int]) -gt 0 -and ($_.GetValue('Type') -as [int]) -ge 16 } |
    ForEach-Object { $_.PSChildName } |
    Where-Object { Get-Service -Name $_ -ErrorAction SilentlyContinue } |
    Select-Object -First 1
if ($protected) {
    $r = Run-Action "menu:$protected" '服务菜单 '
    $line = Line $r.Text '服务菜单 '
    Check "受保护服务 $protected 菜单置灰并注明原因" ($line -match '可修改=false 标题=启动类型（受系统保护') "$line"
} else {
    '跳过 受保护服务置灰：本机没有受保护的 Win32 服务'
}

# 3：普通权限修改被拒绝（只在未提权时做，避免改动真实服务）
if (-not $admin) {
    $before = Start-TypeOf 'RpcSs'
    $r = Run-Action 'set-start:RpcSs:manual' '服务启动类型 '
    $line = Line $r.Text '服务启动类型 '
    Check '普通权限修改启动类型被拒绝（错误码 5）' ($line -match '服务=RpcSs 目标=manual 结果=失败 错误码=5') "$line"
    Check '失败后进程仍在运行' $r.Alive
    $after = Start-TypeOf 'RpcSs'
    Check 'RpcSs 启动类型未改变' (($before -eq $after) -and ($after -match 'AUTO_START')) "前=$before 后=$after"
} else {
    '跳过 普通权限拒绝路径：当前终端已提权'
}

# 4：提权时对临时服务改启动类型并用 sc qc 核对
if ($admin) {
    $svc = 'tml-test-svc'
    if (Get-Service $svc -ErrorAction SilentlyContinue) { throw "已存在服务 $svc，请先确认并手动删除" }
    sc.exe create $svc binPath= "`"$env:SystemRoot\System32\cmd.exe`" /c exit" start= demand DisplayName= "TML 测试服务" | Out-Null
    try {
        $expect = [ordered]@{ 'disabled' = 'DISABLED$'; 'auto-delayed' = 'AUTO_START\s+\(DELAYED\)'; 'auto' = 'AUTO_START$'; 'manual' = 'DEMAND_START$' }
        foreach ($k in $expect.Keys) {
            $r = Run-Action "set-start:${svc}:$k" '服务启动类型 '
            $line = Line $r.Text '服务启动类型 '
            $qc = Start-TypeOf $svc
            Check "临时服务改为 $k" (($line -match "目标=$k 结果=成功") -and ($qc -match $expect[$k])) "$line | sc qc: $qc"
        }
    } finally {
        sc.exe delete $svc | Out-Null
    }
    Check '临时服务已删除' (-not (Get-Service $svc -ErrorAction SilentlyContinue))
} else {
    '跳过 临时服务 sc qc 对比：需要提权终端（本脚本不弹 UAC）'
}

$failed = @($results | Where-Object { -not $_.Ok })
if ($failed.Count) { Write-Host "失败 $($failed.Count) 项"; exit 1 }
Write-Host "全部通过（$($results.Count) 项）"
