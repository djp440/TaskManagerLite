# 测量常驻开销：启动后等待稳定，统计一段时间内本程序占整机 CPU 的百分比与内存。
# 用法：pwsh scripts/measure-idle.ps1 [-Seconds 20] [-Exe target/release/TaskManagerLite.exe]
param(
    [int]$Seconds = 20,
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe"
)

$ErrorActionPreference = 'Stop'
$logDir = Join-Path ([System.IO.Path]::GetTempPath()) "tml-idle-$(Get-Random)"
New-Item -ItemType Directory -Force $logDir | Out-Null
$env:TML_LOG_DIR = $logDir

# --allow-multiple：绕过单实例限制，用户自己的实例在运行时也不会被挡掉。
$p = Start-Process -FilePath (Resolve-Path $Exe).Path -ArgumentList '--allow-multiple' -PassThru
Start-Sleep -Seconds 3
$p.Refresh(); $cpu0 = $p.TotalProcessorTime.TotalMilliseconds
Start-Sleep -Seconds $Seconds
$p.Refresh(); $cpu1 = $p.TotalProcessorTime.TotalMilliseconds
$mem = "工作集 {0:N1} MB，提交（私有）{1:N1} MB" -f ($p.WorkingSet64 / 1MB), ($p.PrivateMemorySize64 / 1MB)
$logText = Get-Content (Get-ChildItem $logDir -Filter *.log | Select-Object -First 1).FullName -Raw
$p.Kill()
Remove-Item -Recurse -Force $logDir

$cores = [Environment]::ProcessorCount
"CPU 占整机 {0:N2}%（{1:N0} ms CPU 时间 / {2} 秒）" -f (($cpu1 - $cpu0) / ($Seconds * 1000 * $cores) * 100), ($cpu1 - $cpu0), $Seconds
$mem
$logText
