# 服务页端到端验收（普通权限）：用 `--service-action <动作>:<服务名>` 启动自己的实例（开发模式，不读写设置、
# 不受单实例限制），首次列出服务后由应用执行动作，结果写进日志；脚本读日志核对：
#   1. 服务页枚举的数量与 Get-Service 一致；
#   2. 对正在运行的 RpcSs 点“启动”：普通权限下得到“拒绝访问”的提示，进程不崩溃（该模式不弹 UAC）；
#   3. 转到进程：跳到进程页并选中 RpcSs 所在的 PID。
# 不会真的启停任何服务：已提权时直接退出（提权下“启动”一个已运行的服务也无害，但仍不冒险）。
# 用法：pwsh scripts/test-services.ps1 [-Exe target/release/TaskManagerLite.exe]
param(
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe",
    [string]$Service = 'RpcSs'
)

$ErrorActionPreference = 'Stop'
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if ($admin) { Write-Host '当前终端已提权：本脚本只在普通权限下运行'; exit 2 }

$exePath = (Resolve-Path $Exe).Path
$results = @()
function Check($label, [bool]$ok, $detail = '') {
    $script:results += [pscustomobject]@{ Name = $label; Ok = $ok; Detail = $detail }
    "{0} {1} {2}" -f ($(if ($ok) { '通过' } else { '失败' })), $label, $detail
}

# 启动一个实例执行动作，等日志出现 $pattern 后结束它，返回日志全文。
function Run-Action($action, $pattern, [int]$seconds = 30) {
    $dir = Join-Path ([System.IO.Path]::GetTempPath()) "tml-services-$(Get-Random)"
    New-Item -ItemType Directory -Force $dir | Out-Null
    $env:TML_LOG_DIR = $dir
    $p = Start-Process $exePath -ArgumentList '--service-action', "${action}:$Service" -PassThru
    $text = ''
    try {
        $end = (Get-Date).AddSeconds($seconds)
        while ((Get-Date) -lt $end) {
            Start-Sleep -Milliseconds 300
            $log = Get-ChildItem $dir -Filter *.log | Select-Object -First 1
            if ($log) { $text = Get-Content $log.FullName -Raw -Encoding utf8 }
            if ($text -match $pattern -or $p.HasExited) { break }
        }
        $alive = -not $p.HasExited
    } finally {
        if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
        Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
    }
    [pscustomobject]@{ Text = $text; Alive = $alive }
}

# 1 + 2：枚举数量与启动被拒
$r = Run-Action 'start' '服务操作 '
$m = [regex]::Matches($r.Text, '服务页 枚举 (\d+) 个服务')
$expected = (Get-Service).Count
$count = if ($m.Count) { [int]$m[$m.Count - 1].Groups[1].Value } else { -1 }
Check '服务数量与 Get-Service 一致' ($count -eq $expected) "应用=$count Get-Service=$expected"
$line = ($r.Text -split "`r?`n" | Where-Object { $_ -match '服务操作 ' } | Select-Object -First 1)
Check '普通权限点击启动有失败提示' ($line -match "无法启动服务.*：") "$line"
Check '操作失败后进程仍在运行' $r.Alive

# 3：转到进程
$pid0 = (Get-CimInstance Win32_Service -Filter "Name='$Service'").ProcessId
$r = Run-Action 'goto' '跳转到进程'
$jump = ($r.Text -split "`r?`n" | Where-Object { $_ -match '跳转到进程' } | Select-Object -First 1)
Check '转到进程选中服务所在 PID' (($jump -match "PID=$pid0 ") -and ($jump -notmatch '行=-1')) "$jump（期望 PID=$pid0）"

$failed = @($results | Where-Object { -not $_.Ok })
if ($failed.Count) { Write-Host "失败 $($failed.Count) 项"; exit 1 }
Write-Host '全部通过'
