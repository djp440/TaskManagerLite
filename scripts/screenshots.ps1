# 生成视觉验收截图：以 --screenshot 模式离屏截图，再把 BMP 转成 PNG。
# 用法：pwsh scripts/screenshots.ps1 [-Out docs/checklist/media/<日期>] [-Exe target/release/TaskManagerLite.exe]
param(
    [string]$Out = "$PSScriptRoot/../docs/checklist/media/$(Get-Date -Format yyyy-MM-dd)",
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe"
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
$env:TML_LOG_DIR = Join-Path ([System.IO.Path]::GetTempPath()) "tml-shots"

# 设置页步骤会切换开机启动与接管任务管理器：改写到 HKCU 下的临时测试键，结束后整棵删除。
$testKey = "Software\TaskManagerLite-test\shots-$PID"
$env:TML_AUTOSTART_TEST_KEY = $testKey
$env:TML_IFEO_TEST_KEY = "$testKey\Taskmgr.exe"
try {
    $p = Start-Process -FilePath (Resolve-Path $Exe).Path -ArgumentList '--screenshot', "`"$Out`"" -PassThru
    $null = $p.Handle  # 先取句柄，进程退出后才能读到退出码
    if (-not $p.WaitForExit(60000)) { $p.Kill(); throw '截图超时' }
} finally {
    Remove-Item Env:TML_AUTOSTART_TEST_KEY, Env:TML_IFEO_TEST_KEY -ErrorAction SilentlyContinue
    Remove-Item "HKCU:\$testKey" -Recurse -Force -ErrorAction SilentlyContinue
}
# 出图前的断言（逻辑处理器曲线尺寸、工具栏宽度）失败时进程会异常退出；工具栏断言的详情写在日志里。
if ($p.ExitCode -ne 0) { throw "截图进程异常退出（退出码 $($p.ExitCode)），日志目录：$env:TML_LOG_DIR" }

foreach ($bmp in Get-ChildItem $Out -Filter *.bmp) {
    $img = [System.Drawing.Image]::FromFile($bmp.FullName)
    $img.Save([System.IO.Path]::ChangeExtension($bmp.FullName, '.png'), [System.Drawing.Imaging.ImageFormat]::Png)
    $img.Dispose()
    Remove-Item $bmp.FullName
}
Get-ChildItem $Out -Filter *.png | ForEach-Object Name
