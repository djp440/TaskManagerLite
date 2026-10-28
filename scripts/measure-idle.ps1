# 测量常驻开销：启动后等待稳定，统计一段时间内本程序占整机 CPU 的百分比与内存。
# 用法：pwsh scripts/measure-idle.ps1 [-Seconds 20] [-View flat|tree|group] [-Exe target/release/TaskManagerLite.exe]
# -View：进程列表的视图模式（通过临时设置目录指定，不读写用户自己的设置）；分组模式会每次采样枚举窗口。
# -AllColumns：显示全部“详细信息”列（含需要后台读取静态信息的用户名、路径、命令行等）。
param(
    [int]$Seconds = 20,
    [ValidateSet('flat', 'tree', 'group')][string]$View = 'flat',
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe",
    [switch]$AllColumns
)

$ErrorActionPreference = 'Stop'
$logDir = Join-Path ([System.IO.Path]::GetTempPath()) "tml-idle-$(Get-Random)"
New-Item -ItemType Directory -Force $logDir | Out-Null
$env:TML_LOG_DIR = $logDir
$settingsDir = Join-Path ([System.IO.Path]::GetTempPath()) "tml-idle-settings-$(Get-Random)"
New-Item -ItemType Directory -Force $settingsDir | Out-Null
$ids = 'status', 'user', 'session', 'priority', 'handles', 'threads', 'working_set', 'peak_working_set', 'commit', 'cpu_time', 'start_time', 'path', 'command_line', 'arch', 'elevated', 'description', 'company'
$details = if ($AllColumns) { @($ids | ForEach-Object { @{ id = $_; width = 100 } }) } else { @() }
@{ view = $View; detail_columns = $details } | ConvertTo-Json -Depth 4 | Set-Content -Path (Join-Path $settingsDir 'settings.json') -Encoding utf8
$env:TML_SETTINGS_DIR = $settingsDir

# --allow-multiple：绕过单实例限制，用户自己的实例在运行时也不会被挡掉。
$p = Start-Process -FilePath (Resolve-Path $Exe).Path -ArgumentList '--allow-multiple' -PassThru
Start-Sleep -Seconds 3
$p.Refresh(); $cpu0 = $p.TotalProcessorTime.TotalMilliseconds
Start-Sleep -Seconds $Seconds
$p.Refresh(); $cpu1 = $p.TotalProcessorTime.TotalMilliseconds
$mem = "工作集 {0:N1} MB，提交（私有）{1:N1} MB" -f ($p.WorkingSet64 / 1MB), ($p.PrivateMemorySize64 / 1MB)
$logText = Get-Content (Get-ChildItem $logDir -Filter *.log | Select-Object -First 1).FullName -Raw
$p.Kill()
Remove-Item -Recurse -Force $logDir, $settingsDir

$cores = [Environment]::ProcessorCount
"视图 {3}$(if ($AllColumns) { '，全部详细信息列' })：CPU 占整机 {0:N2}%（{1:N0} ms CPU 时间 / {2} 秒）" -f (($cpu1 - $cpu0) / ($Seconds * 1000 * $cores) * 100), ($cpu1 - $cpu0), $Seconds, $View
$mem
$logText
