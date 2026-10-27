# 测量启动耗时：多次运行 TaskManagerLite --measure-startup，读取日志中的首帧与首个快照帧耗时（自进程创建起算）。
# 用法：pwsh scripts/measure-startup.ps1 [-Runs 10] [-Exe target/release/TaskManagerLite.exe]
param(
    [int]$Runs = 10,
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe"
)

$ErrorActionPreference = 'Stop'
$exePath = (Resolve-Path $Exe).Path
$logDir = Join-Path ([System.IO.Path]::GetTempPath()) "tml-startup-$(Get-Random)"
New-Item -ItemType Directory -Force $logDir | Out-Null
$env:TML_LOG_DIR = $logDir

$first = @(); $data = @()
for ($i = 1; $i -le $Runs; $i++) {
    Get-ChildItem $logDir -Filter *.log | Remove-Item
    $p = Start-Process -FilePath $exePath -ArgumentList '--measure-startup' -PassThru
    if (-not $p.WaitForExit(15000)) { $p.Kill(); throw "第 $i 次运行超时" }
    $text = Get-Content (Get-ChildItem $logDir -Filter *.log | Select-Object -First 1).FullName -Raw
    if ($text -match '首帧 ([\d.]+) ms') { $first += [double]$Matches[1] }
    if ($text -match '首个快照帧 ([\d.]+) ms') { $data += [double]$Matches[1] }
    # 日志文件名精确到秒，间隔 1 秒避免同名。
    Start-Sleep -Milliseconds 1100
}
Remove-Item -Recurse -Force $logDir

function Median($xs) { $s = $xs | Sort-Object; if ($s.Count -eq 0) { return [double]::NaN }; $s[[int][math]::Floor(($s.Count - 1) / 2)] }
"运行次数: $Runs"
"首帧       中位数 {0:N1} ms  最小 {1:N1} ms  最大 {2:N1} ms" -f (Median $first), ($first | Measure-Object -Minimum).Minimum, ($first | Measure-Object -Maximum).Maximum
"首个快照帧 中位数 {0:N1} ms  最小 {1:N1} ms  最大 {2:N1} ms" -f (Median $data), ($data | Measure-Object -Minimum).Minimum, ($data | Measure-Object -Maximum).Maximum
