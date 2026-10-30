# 进程 -> 服务 跳转的端到端验收（#85，普通权限，不弹 UAC，不启停任何服务，不依赖 SendInput）：
#   1. 对承载多个服务的 svchost：`--menu-action goto-service:<pid>` 跳到服务页，菜单项可用，服务页的行集合与
#      Get-CimInstance Win32_Service 里 ProcessId 相同的服务完全一致，过滤状态与选中项（第一行）正确，保存服务页截图；
#   2. 对只承载一个服务的进程同样验证（体验一致：切到服务页并显示按 PID 过滤）；
#   3. 对自己启动的 ping 子进程（不承载服务）：菜单项置灰，触发后不跳转（仍停在进程页）；
#   4. “服务”列：设置里显示该列时，`--export-csv process=<路径>` 导出的 CSV 中，svchost 行的“服务”单元格等于
#      Win32_Service 的服务名（排序后用“、”连接，超过 6 个截断并注明总数），ping 行为空。
# 钩子等价于右键菜单的 `ProcessMenu.opening` + `goto-service` 回调；每个实例结束后自动退出，不遗留进程。
# 服务页截图（BMP 转 PNG）保存到 -Out（默认 docs/checklist/media/issue-85）。
# 用法：pwsh scripts/test-goto-service.ps1 [-Exe target/release/TaskManagerLite.exe] [-Out <目录>]
param(
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe",
    [string]$Out = "$PSScriptRoot/../docs/checklist/media/issue-85"
)

$ErrorActionPreference = 'Stop'
$exePath = (Resolve-Path $Exe).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$root = Join-Path ([System.IO.Path]::GetTempPath()) "tml-goto-service-$(Get-Random)"
New-Item -ItemType Directory -Force $root | Out-Null
$failures = New-Object System.Collections.Generic.List[string]
function Check($ok, $msg) {
    if ($ok) { "  通过：$msg" } else { "  失败：$msg"; $script:failures.Add($msg) }
}
function Names-Of($pid0) {
    @(Get-CimInstance Win32_Service | Where-Object { $_.ProcessId -eq $pid0 } | ForEach-Object { $_.Name } |
        Sort-Object { $_.ToLowerInvariant() })
}

# 运行一个钩子实例直到它自己退出；返回 @{ Err; Log; Png }。
function Run-Hook($tag, $spec, $settingsJson) {
    $dir = Join-Path $root $tag
    $logs = Join-Path $dir 'logs'; $sets = Join-Path $dir 'settings'
    New-Item -ItemType Directory -Force $logs, $sets | Out-Null
    if ($settingsJson) { Set-Content -Path (Join-Path $sets 'settings.json') -Value $settingsJson -Encoding utf8 }
    $bmp = Join-Path $dir 'shot.bmp'
    $errFile = Join-Path $dir 'stderr.txt'
    $env:TML_LOG_DIR = $logs
    $env:TML_SETTINGS_DIR = $sets
    $env:TML_GOTO_SHOT = $bmp
    $p = Start-Process $exePath -ArgumentList '--menu-action', $spec -PassThru -RedirectStandardError $errFile -RedirectStandardOutput (Join-Path $dir 'stdout.txt')
    try {
        if (-not $p.WaitForExit(60000)) { $p.Kill(); [void]$p.WaitForExit(3000); $script:failures.Add("$tag 钩子应在 60 秒内退出") }
    } finally { if (-not $p.HasExited) { $p.Kill() } }
    $log = Get-ChildItem $logs -Filter *.log -ErrorAction SilentlyContinue | Select-Object -First 1
    [pscustomobject]@{
        Err = if (Test-Path $errFile) { Get-Content $errFile -Raw -Encoding utf8 } else { '' }
        Log = if ($log) { Get-Content $log.FullName -Raw -Encoding utf8 } else { '' }
        Bmp = $bmp
    }
}
function Save-Png($bmp, $name) {
    if (-not (Test-Path $bmp)) { return }
    Add-Type -AssemblyName System.Drawing
    $img = [System.Drawing.Image]::FromFile($bmp)
    try { $img.Save((Join-Path $Out $name), [System.Drawing.Imaging.ImageFormat]::Png) } finally { $img.Dispose() }
}
function Parse-Rows($err) {
    $line = ($err -split "`r?`n" | Where-Object { $_ -match '^服务页行 ' } | Select-Object -First 1)
    if (-not $line) { return @() }
    @(($line -replace '^服务页行 ', '').Split(',', [System.StringSplitOptions]::RemoveEmptyEntries) | ForEach-Object { ($_ -split '=')[0] })
}

# 选目标：svchost 里服务最多的（至少 2 个）、只承载 1 个服务的 svchost，及一个 ping 子进程。
$groups = Get-CimInstance Win32_Service | Where-Object { $_.ProcessId -gt 0 } | Group-Object ProcessId
$live = @{}; Get-Process | ForEach-Object { $live[[int]$_.Id] = $_.ProcessName }
# 优先选全部是“自动启动”的服务的宿主：按需启停的服务会在脚本运行期间退出，造成期望集合变化。
$stable = @($groups | Where-Object { @($_.Group | Where-Object { $_.StartMode -ne 'Auto' }).Count -eq 0 })
function Pick($pred) {
    $pool = @($stable | Where-Object { $live[[int]$_.Name] -eq 'svchost' } | Where-Object $pred)
    if (-not $pool) { $pool = @($groups | Where-Object { $live[[int]$_.Name] -eq 'svchost' } | Where-Object $pred) }
    $pool | Sort-Object Count -Descending | Select-Object -First 1
}
$multi = Pick { $_.Count -ge 2 }
$single = Pick { $_.Count -eq 1 }
$ping = Start-Process ping -ArgumentList '-n', '300', '127.0.0.1' -PassThru -WindowStyle Hidden
try {
    Start-Sleep -Milliseconds 500
    $cases = @()
    if ($multi) { $cases += [pscustomobject]@{ Tag = 'multi'; Pid = [int]$multi.Name } }
    if ($single) { $cases += [pscustomobject]@{ Tag = 'single'; Pid = [int]$single.Name } }
    Check ($cases.Count -gt 0) '找到承载服务的 svchost'

    foreach ($c in $cases) {
        $expected = Names-Of $c.Pid
        "== svchost PID $($c.Pid)（$($c.Tag)，期望 $($expected.Count) 个服务：$($expected -join ', ')）"
        $r = Run-Hook $c.Tag "goto-service:$($c.Pid)"
        Check ($r.Err -match '转到服务 菜单项 enabled=true') '菜单项可用'
        $rows = Parse-Rows $r.Err
        $same = (@($rows | Sort-Object { $_.ToLowerInvariant() }) -join '|') -eq (@($expected) -join '|')
        Check $same "服务页行集合与 Win32_Service 一致（应用 $($rows.Count) 个，期望 $($expected.Count) 个）"
        Check ($r.Err -match "服务页状态 page=4 filter_pid=$($c.Pid) 行数=$($expected.Count) 选中=0 ") '处于服务页、按该 PID 过滤、选中第一行'
        Check ($r.Err -match '提示条="[^"]*PID \d+') '提示条文字带进程名与 PID'
        Check ($r.Log -match "进程页 转到服务 PID=$($c.Pid) 服务数=$($expected.Count)") '日志记录了转到服务'
        Check ($r.Log -match "服务页 按进程过滤 PID=$($c.Pid) 显示 $($expected.Count)/\d+ 选中=0") '日志记录了过滤结果与选中'
        Save-Png $r.Bmp "服务页-按进程过滤-$($c.Tag).png"
    }

    "== 不承载服务的 ping 进程 PID $($ping.Id)"
    $r = Run-Hook 'ping' "goto-service:$($ping.Id)"
    Check ($r.Err -match '转到服务 菜单项 enabled=false') '菜单项置灰'
    Check ($r.Err -match '服务页状态 page=0 filter_pid=0 ') '触发后没有跳转（仍在进程页、没有过滤）'
    Check ($r.Log -match "进程页 转到服务 PID=$($ping.Id) 没有承载服务") '日志记录了拒绝'
    Check ($r.Log -notmatch '服务页 按进程过滤') '没有进入按进程过滤'

    "== 服务列 + CSV 导出"
    $settings = '{"view":"flat","sort":{"key":"name","descending":false},"interval_index":0,"detail_columns":[{"id":"services"}]}'
    $csvFile = Join-Path $root 'process.csv'
    foreach ($c in $cases | Select-Object -First 1) {
        $dir = Join-Path $root 'csv'; $logs = Join-Path $dir 'logs'; $sets = Join-Path $dir 'settings'
        New-Item -ItemType Directory -Force $logs, $sets | Out-Null
        Set-Content -Path (Join-Path $sets 'settings.json') -Value $settings -Encoding utf8
        $env:TML_LOG_DIR = $logs; $env:TML_SETTINGS_DIR = $sets
        $p = Start-Process $exePath -ArgumentList '--export-csv', "`"process=$csvFile`"" -PassThru
        try { if (-not $p.WaitForExit(60000)) { $p.Kill(); $failures.Add('导出钩子应在 60 秒内退出') } } finally { if (-not $p.HasExited) { $p.Kill() } }
    }
    if (Test-Path $csvFile) {
        $csv = Import-Csv $csvFile -Encoding utf8
        $col = ($csv[0].PSObject.Properties.Name | Where-Object { $_ -eq '服务' })
        Check ([bool]$col) 'CSV 带“服务”列'
        $pidCol = ($csv[0].PSObject.Properties.Name | Where-Object { $_ -eq 'PID' } | Select-Object -First 1)
        foreach ($c in $cases) {
            $row = $csv | Where-Object { $_.$pidCol -eq "$($c.Pid)" } | Select-Object -First 1
            $expected = Names-Of $c.Pid
            $want = if ($expected.Count -gt 6) { ($expected[0..5] -join '、') + "…（共 $($expected.Count) 个）" } else { $expected -join '、' }
            Check ($row -and $row.'服务' -eq $want) "svchost PID $($c.Pid) 的服务单元格等于期望（$want）"
        }
        $prow = $csv | Where-Object { $_.$pidCol -eq "$($ping.Id)" } | Select-Object -First 1
        Check ($prow -and $prow.'服务' -eq '') 'ping 行的服务单元格为空'
    } else {
        Check $false 'CSV 文件已生成'
    }
} finally {
    if ($ping -and -not $ping.HasExited) { $ping.Kill() }
    Remove-Item Env:TML_GOTO_SHOT -ErrorAction SilentlyContinue
    Remove-Item $root -Recurse -Force -ErrorAction SilentlyContinue
}

if ($failures.Count) { Write-Host "失败 $($failures.Count) 项"; exit 1 }
Write-Host '全部通过'
Write-Host "截图：$Out"
