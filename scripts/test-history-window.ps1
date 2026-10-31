# 性能页曲线窗口（#86）端到端验收：不依赖 SendInput，用临时设置目录（TML_SETTINGS_DIR）与日志目录启动自己的实例（--allow-multiple）。
#   1. 四种窗口（60s / 5m / 10m / 1h）分别启动、运行一段时间后 WM_CLOSE：核对日志里的窗口、窗口内点数 / 时间跨度 / 估算内存，
#      设置文件写回 history_window 且保留未知字段。
#   2. 设置缺字段（旧版本）或值不认识 -> 按 60 秒处理，并在保存时补上字段。
#   3. 采样中断留空：5 分钟窗口下最小化若干秒再还原，后台采样关闭时窗口内有缺口，开启时没有缺口。
#   4. 迷你模式保持 60 秒：设置里是 1 小时，迷你模式下实际绘制窗口仍是 60 秒。
# 只结束自己启动的进程。用法：pwsh scripts/test-history-window.ps1 [-RunSeconds 25] [-Exe target/release/TaskManagerLite.exe]
param(
    [int]$RunSeconds = 25,
    [int]$HiddenSeconds = 6,
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe"
)

$ErrorActionPreference = 'Stop'
$exePath = (Resolve-Path $Exe).Path
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class Win {
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
}
'@

$root = Join-Path ([System.IO.Path]::GetTempPath()) "tml-histwin-$(Get-Random)"
New-Item -ItemType Directory -Force $root | Out-Null
$failures = New-Object System.Collections.Generic.List[string]
function Check($ok, $msg) {
    if ($ok) { "  通过：$msg" } else { "  失败：$msg"; $script:failures.Add($msg) }
}
function Field($line, $key) { if ($line -match "$key=(-?\d+)") { [int64]$Matches[1] } else { -1 } }
function Word($line, $key) { if ($line -match "$key=(\S+)") { $Matches[1] } else { '' } }

# 启动实例，等主窗口；返回 @{Process; Handle; SettingsDir; LogDir}。
function Start-App($name, $json, $extraArgs = @()) {
    $settingsDir = Join-Path $root "settings-$name"
    $logDir = Join-Path $root "logs-$name"
    New-Item -ItemType Directory -Force $settingsDir, $logDir | Out-Null
    if ($null -ne $json) { Set-Content -Path (Join-Path $settingsDir 'settings.json') -Value $json -Encoding utf8 }
    $env:TML_SETTINGS_DIR = $settingsDir
    $env:TML_LOG_DIR = $logDir
    $p = Start-Process -FilePath $exePath -ArgumentList (@('--allow-multiple') + $extraArgs) -PassThru
    $h = [IntPtr]::Zero
    for ($i = 0; $i -lt 150 -and $h -eq [IntPtr]::Zero; $i++) {
        Start-Sleep -Milliseconds 100
        $p.Refresh()
        if ($p.HasExited) { throw "进程提前退出，退出码 $($p.ExitCode)" }
        $h = $p.MainWindowHandle
    }
    if ($h -eq [IntPtr]::Zero) { $p.Kill(); throw '等不到主窗口' }
    @{ Process = $p; Handle = $h; SettingsDir = $settingsDir; LogDir = $logDir }
}

function Close-App($app) {
    [void][Win]::PostMessage($app.Handle, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) # WM_CLOSE
    if (-not $app.Process.WaitForExit(10000)) { $app.Process.Kill(); throw '收到 WM_CLOSE 后 10 秒内未退出' }
}

function Log-Lines($app) {
    $log = Get-ChildItem $app.LogDir -Filter *.log | Sort-Object LastWriteTime | Select-Object -Last 1
    (Get-Content $log.FullName -Raw -Encoding utf8) -split "`r?`n"
}

function Saved-Settings($app) { Get-Content (Join-Path $app.SettingsDir 'settings.json') -Raw -Encoding utf8 | ConvertFrom-Json }

# 每个窗口在运行 $RunSeconds 秒（1 秒间隔）后至少应有的点数：原始层每秒一点，聚合层按桶宽 2 / 4 / 20 秒。
function Expected-Points($key, $secs) {
    switch ($key) {
        '60s' { $secs - 4 }
        '5m' { [math]::Floor($secs / 2) - 3 }
        '10m' { [math]::Floor($secs / 4) - 2 }
        '1h' { [math]::Floor($secs / 20) + 1 }
    }
}

$apps = @()
try {
    foreach ($key in '60s', '5m', '10m', '1h') {
        "== 1. 窗口 $key：运行 $RunSeconds 秒后关闭 =="
        $app = Start-App "w-$key" "{`"history_window`": `"$key`", `"future_field`": {`"keep`": [1, 2, 3]}}"
        $apps += $app
        Start-Sleep -Seconds $RunSeconds
        $app.Process.Refresh()
        $ws = $app.Process.WorkingSet64 / 1MB
        Close-App $app
        $lines = Log-Lines $app
        Check ($lines -match "曲线窗口 $key$") "启动日志记录曲线窗口 $key"
        $report = $lines | Where-Object { $_ -match '曲线历史 保存设置' } | Select-Object -Last 1
        "  日志：$report"
        "  运行期间工作集 {0:N1} MB" -f $ws
        Check ($null -ne $report) '关闭时日志里有“曲线历史”记录'
        Check ((Word $report '窗口') -eq $key -and (Word $report '绘制窗口') -eq $key) "窗口与绘制窗口都是 $key"
        $points = Field $report '窗口点数'
        $want = Expected-Points $key $RunSeconds
        Check ($points -ge $want) "窗口内点数 $points 不少于 $want"
        Check ((Field $report '窗口缺口') -eq 0) '连续运行没有缺口'
        $span = Field $report '跨度'
        if ($key -ne '1h') {
            Check ($span -ge ($RunSeconds - 6) * 1000) "时间跨度 $span ms 覆盖整个运行期间（至少 $($RunSeconds - 6) 秒）"
        } else {
            Check ($span -ge 0) "1 小时窗口 20 秒一个点，跨度 $span ms（不足 40 秒时只有 1～2 个点）"
        }
        $mem = Field $report '全部曲线估算内存'
        Check ($mem -gt 0 -and $mem -lt 2MB) "全部曲线历史估算内存 $mem 字节（< 2 MB）"
        $s = Saved-Settings $app
        Check ($s.history_window -eq $key) "设置文件写回 history_window = $key"
        Check ($s.future_field.keep.Count -eq 3) '未知字段 future_field 原样保留'
    }

    "== 2. 旧版设置缺字段 / 值不认识 -> 60 秒 =="
    foreach ($case in @(@('missing', '{"view": "tree"}'), @('bad', '{"history_window": "2h"}'), @('type', '{"history_window": 3}'))) {
        $app = Start-App "d-$($case[0])" $case[1]
        $apps += $app
        Start-Sleep -Seconds 3
        Close-App $app
        $lines = Log-Lines $app
        Check ($lines -match '曲线窗口 60s$') "$($case[0])：按 60 秒处理"
        Check ((Saved-Settings $app).history_window -eq '60s') "$($case[0])：保存时补上 history_window = 60s"
    }

    "== 3. 采样中断留空：5 分钟窗口，最小化 $HiddenSeconds 秒 =="
    foreach ($background in $false, $true) {
        $name = if ($background) { 'on' } else { 'off' }
        $app = Start-App "gap-$name" "{`"history_window`": `"5m`", `"background_sampling`": $($background.ToString().ToLower())}"
        $apps += $app
        Start-Sleep -Seconds 4
        [void][Win]::ShowWindow($app.Handle, 6)   # SW_MINIMIZE
        Start-Sleep -Milliseconds 1500
        if (-not [Win]::IsIconic($app.Handle)) { throw '窗口没有最小化' }
        Start-Sleep -Seconds $HiddenSeconds
        [void][Win]::ShowWindow($app.Handle, 9)   # SW_RESTORE
        Start-Sleep -Seconds 3
        $report = Log-Lines $app | Where-Object { $_ -match '恢复可见' } | Select-Object -Last 1
        "  后台采样$name 日志：$report"
        Check ($null -ne $report -and (Word $report '窗口') -eq '5m') '还原后日志里有 5 分钟窗口的“恢复可见”记录'
        if ($background) {
            Check ((Field $report '窗口缺口') -eq 0) '后台采样开启：5 分钟窗口没有缺口'
        } else {
            Check ((Field $report '窗口缺口') -ge 1) '后台采样关闭：5 分钟窗口在最小化处留下缺口（曲线断开）'
            Check ((Field $report '跨度') -ge ($HiddenSeconds * 1000)) '时间轴包含最小化的整段时间（跨度不小于最小化时长）'
        }
        $app.Process.Kill()
    }

    "== 4. 迷你模式保持 60 秒：设置为 1 小时 =="
    $app = Start-App "mini" '{"history_window": "1h", "mini": {"active": true}}'
    $apps += $app
    $line = $null
    for ($i = 0; $i -lt 50 -and $null -eq $line; $i++) {
        Start-Sleep -Milliseconds 200
        $line = Log-Lines $app | Where-Object { $_ -match '进入迷你模式' } | Select-Object -Last 1
    }
    "  日志：$line"
    Check ($null -ne $line) '以迷你模式启动，日志里有“进入迷你模式”'
    Check ((Word $line '绘制窗口') -eq '60s' -and (Word $line '设置窗口') -eq '1h') '迷你模式绘制 60 秒，设置里的 1 小时不受影响'
    $app.Process.Kill()

    "== 5. 灌入 1 小时合成历史（--history-prefill 3600）：点数上限与内存 =="
    # 各窗口的标称点数：原始层 0.5 秒间隔 60 秒 = 121 点（运行中又追加几个），聚合层 150 / 150 / 180 点。
    foreach ($case in @(@('60s', 121, 60), @('5m', 150, 300), @('10m', 150, 600), @('1h', 180, 3600))) {
        $key = $case[0]
        $app = Start-App "fill-$key" "{`"history_window`": `"$key`"}" @('--history-prefill', '3600')
        $apps += $app
        Start-Sleep -Seconds 8
        $app.Process.Refresh()
        $ws = $app.Process.WorkingSet64 / 1MB
        Close-App $app
        $lines = Log-Lines $app
        $report = $lines | Where-Object { $_ -match '曲线历史 保存设置' } | Select-Object -Last 1
        "  $key 日志：$report"
        "  $key 填满后工作集 {0:N1} MB" -f $ws
        Check ($null -ne ($lines | Where-Object { $_ -match '灌入合成数据 3600 秒' })) "$key：灌入了合成历史"
        $points = Field $report '窗口点数'
        Check ($points -ge $case[1] - 6 -and $points -le $case[1] + 12) "$key：窗口点数 $points 在标称 $($case[1]) 附近（有上限，不随采样个数增长）"
        Check ((Field $report '跨度') -ge $case[2] * 900) "$key：时间轴铺满窗口（跨度 $(Field $report '跨度') ms）"
        $mem = Field $report '全部曲线估算内存'
        Check ($mem -gt 0 -and $mem -lt 2MB) "$key：全部曲线历史估算内存 $mem 字节（< 2 MB）"
    }
}
finally {
    foreach ($a in $apps) { if ($a.Process -and -not $a.Process.HasExited) { $a.Process.Kill() } }
    Start-Sleep -Milliseconds 300
    Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}

if ($failures.Count -gt 0) { "失败 $($failures.Count) 项"; exit 1 }
'全部通过'
