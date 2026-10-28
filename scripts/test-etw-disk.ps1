# ETW 磁盘速率端到端验收（需要提权终端运行；未提权直接退出，不尝试提权）。
# 用法：pwsh scripts/test-etw-disk.ps1 [-SizeMB 512] [-NoBuild]
#
# 检查：
#   1. 写大文件时，探针（examples/disk_probe）的 ETW 来源（source=etw）把字节数归属到写文件的进程，
#      写文件进程 + System（惰性写入器）合计不少于文件大小的 80%；同时输出近似速率（--no-etw）作对比。
#   2. 探针正常退出后，本程序的 ETW 会话（名称前缀 TaskManagerLite-DiskIo-）已消失。
#   3. 探针被强行结束后会话残留（这是预期的）；下一次启动探针会清理它。
# 只创建并删除自己的临时文件，只结束自己启动的进程。
param(
    [int]$SizeMB = 512,
    [switch]$NoBuild
)

$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
$identity = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $identity.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Host '需要在提权（管理员）终端运行；本脚本不会尝试提权。' -ForegroundColor Yellow
    exit 2
}

if (-not $NoBuild) {
    Push-Location $root
    cargo build -p tml-core --release --example disk_probe
    if ($LASTEXITCODE -ne 0) { throw '构建探针失败' }
    Pop-Location
}
$probe = Join-Path $root 'target/release/examples/disk_probe.exe'
if (-not (Test-Path $probe)) { throw "找不到 $probe" }

$work = Join-Path ([IO.Path]::GetTempPath()) "tml-etw-$(Get-Random)"
New-Item -ItemType Directory -Force $work | Out-Null
$failures = @()
$started = @()

function Get-Sessions { @(logman query -ets 2>$null | Select-String 'TaskManagerLite-DiskIo-' | ForEach-Object { ($_.Line -split '\s+')[0] }) }
function Check($ok, $msg) {
    if ($ok) { Write-Host "通过：$msg" -ForegroundColor Green } else { Write-Host "失败：$msg" -ForegroundColor Red; $script:failures += $msg }
}

# 写文件的子进程：WriteThrough 让写入在发起线程里同步落盘，尽量不经惰性写入器。
$writer = Join-Path $work 'writer.ps1'
@'
param($Path, $MB)
$buf = New-Object byte[] (1MB)
(New-Object Random).NextBytes($buf)
$fs = New-Object IO.FileStream($Path, 'Create', 'Write', 'None', 1MB, [IO.FileOptions]::WriteThrough)
for ($i = 0; $i -lt $MB; $i++) { $fs.Write($buf, 0, $buf.Length) }
$fs.Dispose()
'@ | Set-Content -Path $writer -Encoding utf8

function Run-Probe([string[]]$extra, [int]$secs, [bool]$withWriter) {
    $csv = Join-Path $work "probe-$(Get-Random).csv"
    $p = Start-Process -FilePath $probe -ArgumentList (@('--secs', $secs) + $extra) -RedirectStandardOutput $csv -PassThru -WindowStyle Hidden
    $script:started += $p
    $result = [ordered]@{ Probe = $p; Csv = $csv; WriterPid = 0 }
    if ($withWriter) {
        Start-Sleep -Seconds 5 # 等会话就绪（首次采样之后才启动）并丢弃第一个半截窗口
        $target = Join-Path $work 'big.bin'
        $w = Start-Process -FilePath 'pwsh' -ArgumentList '-NoProfile', '-File', $writer, $target, $SizeMB -PassThru -WindowStyle Hidden
        $script:started += $w
        $result.WriterPid = $w.Id
        $w.WaitForExit()
        Remove-Item -Force $target -ErrorAction SilentlyContinue
    }
    $p.WaitForExit()
    $result.Rows = @(Import-Csv $csv)
    $result
}

try {
    $before = Get-Sessions
    Check ($before.Count -eq 0) "开始前没有本程序的 ETW 会话（现有：$($before -join ', ')）"

    # 1. ETW 来源
    $r = Run-Probe @() 30 $true
    $etw = @($r.Rows | Where-Object { $_.source -eq 'etw' })
    Check ($etw.Count -gt 0) "探针在提权下使用了 ETW 来源（etw 行数 $($etw.Count)）"
    $bytes = { param($rows, $procId) [double](($rows | Where-Object { [int]$_.pid -eq $procId } | Measure-Object bytes_per_sec -Sum).Sum) }
    $w = & $bytes $etw $r.WriterPid
    $sys = & $bytes $etw 4
    $total = $SizeMB * 1MB
    "写文件进程 PID $($r.WriterPid)：ETW 累计 {0:N0} MB；System：{1:N0} MB；文件 {2} MB" -f ($w / 1MB), ($sys / 1MB), $SizeMB
    Check (($w + $sys) -ge 0.8 * $total) "写文件进程 + System 的 ETW 字节数不少于文件大小的 80%"

    # 近似来源对比
    $a = Run-Probe @('--no-etw') 30 $true
    $aw = & $bytes @($a.Rows | Where-Object { $_.source -eq 'approx' }) $a.WriterPid
    "近似来源（--no-etw）下写文件进程累计：{0:N0} MB（含非磁盘 I/O，通常不低于 ETW 值）" -f ($aw / 1MB)

    # 2. 正常退出后会话消失
    $left = Get-Sessions
    Check ($left.Count -eq 0) "探针正常退出后没有残留会话（现有：$($left -join ', ')）"

    # 3. 强行结束后残留，下次启动清理
    $p = Start-Process -FilePath $probe -ArgumentList '--secs', 60 -RedirectStandardOutput (Join-Path $work 'k.csv') -PassThru -WindowStyle Hidden
    $started += $p
    Start-Sleep -Seconds 5
    $during = Get-Sessions
    Check ($during.Count -eq 1 -and $during[0] -like "*-$($p.Id)-*") "探针运行时存在唯一会话，名称带其 PID $($p.Id)（现有：$($during -join ', ')）"
    Stop-Process -Id $p.Id -Force
    $p.WaitForExit()
    $orphan = Get-Sessions
    Check ($orphan.Count -eq 1) "探针被强行结束后会话残留（预期，等待下次启动清理）"
    $r2 = Run-Probe @() 6 $false
    $after = Get-Sessions
    Check ($after.Count -eq 0) "再次启动探针并退出后，残留会话已被清理（现有：$($after -join ', ')）"
}
finally {
    foreach ($p in $started) { if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue } }
    # 只清理本脚本创建的残留会话（名称里的 PID 属于本脚本启动的探针）。
    foreach ($s in Get-Sessions) {
        if ($started | Where-Object { $s -like "*-$($_.Id)-*" }) { logman stop $s -ets | Out-Null }
    }
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

if ($failures.Count -gt 0) {
    Write-Host "`n$($failures.Count) 项失败" -ForegroundColor Red
    exit 1
}
Write-Host "`n全部通过" -ForegroundColor Green
