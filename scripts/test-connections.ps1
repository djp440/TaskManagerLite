# 连接页（#77）端到端验收（普通权限即可，不用 SendInput）。脚本自己开一个 TCP 监听端口并连上它，
# 用 `--allow-multiple --connections-action <动作>` 启动自己的实例（开发模式，不读写设置），环境变量
# TML_CONNECTIONS_DUMP 让应用每次刷新把全部连接写到临时文件；脚本读文件与日志核对：
#   1. 监听端口集合与 Get-NetTCPConnection / Get-NetUDPEndpoint 基本一致（容错：允许瞬时差异，要求大部分一致）；
#   2. 脚本自己的监听端口与已建立连接出现在列表里，且 PID 为脚本进程；
#   3. 连接页可见时随采样持续刷新；
#   4. “转到进程”钩子切到进程页并选中脚本进程的 PID；切走之后日志里不再出现连接页刷新；
#   5. 截取连接页窗口图（PrintWindow，窗口会短暂出现在屏幕上）到 -Out。
# 结束时只结束自己启动的实例。
# 用法：pwsh scripts/test-connections.ps1 [-Exe target/release/TaskManagerLite.exe] [-Out docs/checklist/media/issue-77]
param(
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe",
    [string]$Out = "$PSScriptRoot/../docs/checklist/media/issue-77"
)

$ErrorActionPreference = 'Stop'
$exePath = (Resolve-Path $Exe).Path
$results = @()
function Check($label, [bool]$ok, $detail = '') {
    $script:results += [pscustomobject]@{ Name = $label; Ok = $ok; Detail = $detail }
    "{0} {1} {2}" -f ($(if ($ok) { '通过' } else { '失败' })), $label, $detail
}

Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class ShotWin {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
"@
# 物理像素坐标截图（否则高 DPI 下窗口矩形被缩放，截图不全）。
[void][ShotWin]::SetProcessDPIAware()
function Save-Window([IntPtr]$h, [string]$path) {
    $r = New-Object ShotWin+RECT; [void][ShotWin]::GetWindowRect($h, [ref]$r)
    $bmp = New-Object System.Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
    $g = [System.Drawing.Graphics]::FromImage($bmp); $dc = $g.GetHdc()
    [void][ShotWin]::PrintWindow($h, $dc, 2)
    $g.ReleaseHdc($dc); $g.Dispose()
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
}

# 地址规范化：去掉 IPv6 的方括号与 %scope。
function Split-Endpoint([string]$ep) {
    $i = $ep.LastIndexOf(':')
    $addr = $ep.Substring(0, $i).Trim('[', ']') -replace '%.*$', ''
    [pscustomobject]@{ Addr = $addr; Port = [int]$ep.Substring($i + 1) }
}

function Read-Dump($path) {
    if (-not (Test-Path $path)) { return @() }
    Get-Content $path -Encoding utf8 | Where-Object { $_ } | ForEach-Object {
        $f = $_ -split "`t"
        $l = Split-Endpoint $f[1]
        [pscustomobject]@{ Proto = $f[0]; Addr = $l.Addr; Port = $l.Port; Remote = $f[2]; State = $f[3]; Pid = [int]$f[4]; Name = $f[5] }
    }
}

# 启动一个实例执行动作；等日志匹配 $pattern 后再等 $linger 秒，返回日志全文、数据文件与窗口截图结果。
function Run-Action($action, $pattern, [int]$linger = 0, [string]$shot = '', [int]$seconds = 30) {
    $dir = Join-Path ([System.IO.Path]::GetTempPath()) "tml-connections-$(Get-Random)"
    New-Item -ItemType Directory -Force $dir | Out-Null
    $env:TML_LOG_DIR = $dir
    $env:TML_CONNECTIONS_DUMP = Join-Path $dir 'connections.tsv'
    $env:TML_INSTANCE_ID = "connections$(Get-Random)"
    $p = Start-Process $exePath -ArgumentList '--allow-multiple', '--connections-action', $action -PassThru
    $text = ''; $rows = @()
    try {
        $end = (Get-Date).AddSeconds($seconds)
        while ((Get-Date) -lt $end) {
            Start-Sleep -Milliseconds 300
            $log = Get-ChildItem $dir -Filter *.log | Select-Object -First 1
            if ($log) { $text = Get-Content $log.FullName -Raw -Encoding utf8 }
            if ($text -match $pattern -or $p.HasExited) { break }
        }
        if ($linger -gt 0) {
            Start-Sleep -Seconds $linger
            $log = Get-ChildItem $dir -Filter *.log | Select-Object -First 1
            if ($log) { $text = Get-Content $log.FullName -Raw -Encoding utf8 }
        }
        if ($shot -and -not $p.HasExited) {
            $p.Refresh()
            if ($p.MainWindowHandle -ne [IntPtr]::Zero) {
                New-Item -ItemType Directory -Force (Split-Path $shot) | Out-Null
                Save-Window $p.MainWindowHandle $shot
            }
        }
        $rows = @(Read-Dump $env:TML_CONNECTIONS_DUMP)
        $alive = -not $p.HasExited
    } finally {
        if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }
        Remove-Item Env:TML_CONNECTIONS_DUMP, Env:TML_INSTANCE_ID -ErrorAction SilentlyContinue
        Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
    }
    [pscustomobject]@{ Text = $text; Rows = $rows; Alive = $alive }
}

# 脚本自己的监听端口与一条已建立连接（PID = 本 pwsh 进程）。
$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
$listener.Start()
$port = $listener.LocalEndpoint.Port
$client = [System.Net.Sockets.TcpClient]::new()
$client.Connect([System.Net.IPAddress]::Loopback, $port)
$server = $listener.AcceptTcpClient()
$clientPort = $client.Client.LocalEndPoint.Port
"脚本 PID=$PID 监听端口=$port 客户端端口=$clientPort"

try {
    # 1–3：连接页停留几秒，读数据文件并截图。
    $shotPath = if ($Out) { Join-Path (Resolve-Path -LiteralPath (New-Item -ItemType Directory -Force $Out)).Path '01-连接页.png' } else { '' }
    $r = Run-Action 'dump' '连接页 测试钩子 dump' 4 $shotPath
    Check '应用正常运行' $r.Alive
    $refreshes = ([regex]::Matches($r.Text, '连接页 枚举 \d+ 条连接')).Count
    Check '连接页可见时持续刷新' ($refreshes -ge 3) "刷新 $refreshes 次"
    $rows = $r.Rows
    Check '读到连接数据' ($rows.Count -gt 0) "共 $($rows.Count) 条"

    $mine = $rows | Where-Object { $_.Proto -eq 'TCP' -and $_.Port -eq $port -and $_.State -eq 'Listen' }
    Check '脚本的监听端口出现且 PID 正确' (@($mine | Where-Object { $_.Pid -eq $PID }).Count -eq 1) "$(@($mine | ForEach-Object { "PID=$($_.Pid) 名称=$($_.Name)" }) -join '; ')"
    $est = $rows | Where-Object { $_.Proto -eq 'TCP' -and $_.Port -eq $clientPort -and $_.State -eq 'Established' -and $_.Remote -eq "127.0.0.1:$port" }
    Check '脚本的已建立连接出现且 PID 正确' (@($est | Where-Object { $_.Pid -eq $PID }).Count -eq 1)

    # 与 netstat -ano 对比（issue 的验收口径）：(协议, 地址, 端口, PID)，允许少量瞬时差异。
    $ns = @(netstat -ano | ForEach-Object {
        $f = ($_.Trim() -split '\s+')
        if ($f[0] -eq 'TCP' -or $f[0] -eq 'UDP') {
            $l = Split-Endpoint $f[1]
            $v6 = $f[1].StartsWith('[')
            $proto = $f[0] + $(if ($v6) { '6' } else { '' })
            [pscustomobject]@{ Proto = $proto; Key = "$proto|$($l.Addr)|$($l.Port)|$($f[-1])"; Listen = ($f[0] -eq 'TCP' -and $f[3] -eq 'LISTENING') }
        }
    })
    $key = { "$($_.Proto)|$($_.Addr)|$($_.Port)|$($_.Pid)" }
    function Compare-Set($label, $app, $sys, [double]$min) {
        $common = @($app | Where-Object { $sys -contains $_ }).Count
        $ratio = if ($sys.Count -or $app.Count) { $common / [Math]::Max(1, [Math]::Max($sys.Count, $app.Count)) } else { 1 }
        Check $label ($ratio -ge $min) ("应用 {0} netstat {1} 共同 {2}（{3:P0}）" -f $app.Count, $sys.Count, $common, $ratio)
    }
    Compare-Set 'TCP 监听端口与 netstat -ano 一致' @($rows | Where-Object { $_.State -eq 'Listen' } | ForEach-Object $key | Sort-Object -Unique) @($ns | Where-Object Listen | ForEach-Object Key | Sort-Object -Unique) 0.95
    Compare-Set 'UDP 端点与 netstat -ano 一致' @($rows | Where-Object { $_.Proto -like 'UDP*' } | ForEach-Object $key | Sort-Object -Unique) @($ns | Where-Object { $_.Proto -like 'UDP*' } | ForEach-Object Key | Sort-Object -Unique) 0.9
    # Get-NetTCPConnection 会漏掉部分监听（如部分系统组件），只要求它的结果都在应用列表里。
    $appListen = @($rows | Where-Object { $_.State -eq 'Listen' } | ForEach-Object { "$($_.Addr)|$($_.Port)|$($_.Pid)" })
    $sysListen = @(Get-NetTCPConnection -State Listen | ForEach-Object { "$($_.LocalAddress -replace '%.*$', '')|$($_.LocalPort)|$($_.OwningProcess)" } | Sort-Object -Unique)
    $missing = @($sysListen | Where-Object { $appListen -notcontains $_ }).Count
    Check 'Get-NetTCPConnection 的监听端口都在列表里' ($missing -le [Math]::Max(1, [int]($sysListen.Count * 0.05))) "缺 $missing / $($sysListen.Count)"
    $protos = ($rows | Group-Object Proto | ForEach-Object { "$($_.Name)=$($_.Count)" }) -join ' '
    Check '同时读到 IPv4 与 IPv6 的表' ((@($rows | Where-Object { $_.Proto -eq 'TCP' }).Count -gt 0) -and (@($rows | Where-Object { $_.Proto -like '*6' }).Count -gt 0)) $protos
    Check '日志不含地址' ($r.Text -notmatch '127\.0\.0\.1|0\.0\.0\.0') ''

    # 4：转到进程，切走后不再刷新。
    $r = Run-Action "goto:${PID}:$port" '连接页 跳转到进程' 4
    $jump = ($r.Text -split "`r?`n" | Where-Object { $_ -match '连接页 跳转到进程' } | Select-Object -First 1)
    Check '转到进程选中脚本进程' (($jump -match "PID=$PID ") -and ($jump -match "选中PID=$PID$") -and ($jump -notmatch '行=-1')) "$jump"
    $after = $r.Text.Substring([Math]::Max(0, $r.Text.IndexOf('连接页 跳转到进程')))
    $later = ([regex]::Matches($after, '连接页 枚举 \d+ 条连接')).Count
    Check '离开连接页后不再刷新（4 秒内）' ($later -eq 0) "跳转后刷新 $later 次"
    Check '离开后切到了进程页' ($r.Text -match '界面事件 page=5[\s\S]*界面事件 page=0') ''
} finally {
    $server.Dispose(); $client.Dispose(); $listener.Stop()
}

$failed = @($results | Where-Object { -not $_.Ok })
if ($failed.Count) { Write-Host "失败 $($failed.Count) 项"; exit 1 }
Write-Host '全部通过'
