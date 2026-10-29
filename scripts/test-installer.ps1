# 安装包端到端验收（见 docs/adr/0005-installer.md）。编译测试构建的安装包，按用户安装（/CURRENTUSER）到临时目录，全程不提权。
#   - TML_INSTANCE_ID 隔离单实例互斥量（测试构建的 AppMutex 带同样后缀），机器上已有真实实例也不受影响；
#   - TML_IFEO_TEST_KEY / TML_AUTOSTART_TEST_KEY 把接管登记与开机启动改写到 HKCU\Software\TaskManagerLite-test\ 下，不碰 HKLM 与真实 Run 键；
#   - 测试构建的 AppId、快捷方式名另起，不影响真实安装；开始菜单里会短暂出现“TaskManagerLite (test …)”，卸载时删除。
# 覆盖：实例运行时拒绝安装；勾选接管但已被其他程序占用时不覆盖；勾选接管写入带引号的路径；不勾选的覆盖安装保留登记；
#       卸载时撤销接管、删除指向本安装的开机启动登记、删除程序文件与快捷方式。
# 用法：pwsh scripts/test-installer.ps1 [-Exe target/release/TaskManagerLite.exe]
param([string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe")

$ErrorActionPreference = 'Stop'
$id = "inst-$PID-$(Get-Random)"
$testBase = "Software\TaskManagerLite-test\$id"
$ifeoKey = "HKCU:\$testBase\Taskmgr.exe"
$autoBase = "$testBase-autostart"
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) "tml-installer-$id"
$appDir = Join-Path $tmp 'app'
$appExe = Join-Path $appDir 'TaskManagerLite.exe'
$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) "TaskManagerLite (test $id).lnk"
$failures = New-Object System.Collections.Generic.List[string]
New-Item -ItemType Directory -Force $tmp | Out-Null

function Check($ok, $msg) {
    if ($ok) { "  通过：$msg" } else { "  失败：$msg"; $script:failures.Add($msg) }
}
function Debugger { (Get-ItemProperty -Path $ifeoKey -Name Debugger -ErrorAction SilentlyContinue).Debugger }
function AutostartValue($sub) { (Get-ItemProperty -Path "HKCU:\$autoBase\$sub" -Name TaskManagerLite -ErrorAction SilentlyContinue).TaskManagerLite }

$step = 0
function Install([string]$tasks) {
    $script:step++
    $argList = @('/CURRENTUSER', '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/DIR=$appDir", "/TASKS=$tasks", "/LOG=$tmp\setup-$script:step.log")
    (Start-Process -FilePath $setup -ArgumentList $argList -PassThru -Wait).ExitCode
}

$instance = $null
try {
    $env:TML_INSTANCE_ID = $id
    $env:TML_IFEO_TEST_KEY = "$testBase\Taskmgr.exe"
    $env:TML_AUTOSTART_TEST_KEY = $autoBase
    $env:TML_SETTINGS_DIR = Join-Path $tmp 'settings'
    $env:TML_LOG_DIR = Join-Path $tmp 'logs'

    "== 编译测试构建的安装包"
    $built = & "$PSScriptRoot/build-installer.ps1" -Exe $Exe -TestId $id
    $built
    $setup = ($built | Select-Object -Last 1) -replace '（.*$', ''

    "== 实例运行时拒绝安装"
    $instance = Start-Process -FilePath (Resolve-Path $Exe).Path -PassThru
    $deadline = (Get-Date).AddSeconds(10)
    # 主窗口出现时单实例互斥量早已创建。
    do { Start-Sleep -Milliseconds 200; $instance.Refresh() } while ((Get-Date) -lt $deadline -and $instance.MainWindowHandle -eq 0)
    Check ((Install '') -ne 0) '安装程序以非零退出码结束'
    # 互斥量检测失败时提示“正在运行”，静默模式下按“取消”处理，日志记 EAbort（区别于其他原因的失败）。
    Check (Select-String -Quiet -SimpleMatch 'Got EAbort exception' "$tmp\setup-$step.log") '是因检测到运行中的实例而中止'
    Check (-not (Test-Path $appExe)) '没有写入程序文件'
    Stop-Process -Id $instance.Id -Force
    $instance.WaitForExit()
    $instance = $null

    "== 勾选接管，但已被其他程序占用"
    $other = "$env:SystemRoot\System32\notepad.exe"
    New-Item -Force $ifeoKey | Out-Null
    Set-ItemProperty -Path $ifeoKey -Name Debugger -Value "`"$other`""
    Check ((Install 'replacetaskmgr') -eq 0) '安装成功'
    Check (Test-Path $appExe) '程序文件已安装'
    Check (Test-Path $shortcut) '开始菜单快捷方式已创建'
    Check ((Debugger) -eq "`"$other`"") "静默安装不覆盖其他程序的登记（当前：$(Debugger)）"

    "== 未被占用时勾选接管"
    Remove-ItemProperty -Path $ifeoKey -Name Debugger
    Check ((Install 'replacetaskmgr') -eq 0) '覆盖安装成功'
    Check ((Debugger) -eq "`"$appExe`"") "登记指向安装位置并带引号（当前：$(Debugger)）"

    "== 不勾选接管的覆盖安装"
    Check ((Install '') -eq 0) '覆盖安装成功'
    Check ((Debugger) -eq "`"$appExe`"") '原有登记保留'

    "== 卸载"
    New-Item -Force "HKCU:\$autoBase\Run", "HKCU:\$autoBase\Approved" | Out-Null
    Set-ItemProperty -Path "HKCU:\$autoBase\Run" -Name TaskManagerLite -Value "`"$appExe`" --autostart"
    Set-ItemProperty -Path "HKCU:\$autoBase\Approved" -Name TaskManagerLite -Value ([byte[]](2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)) -Type Binary
    # 卸载程序会把自身复制到临时目录再运行，原进程先退出，所以按程序文件被删除来判断完成。
    Start-Process -FilePath (Join-Path $appDir 'unins000.exe') -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=$tmp\uninstall.log" -Wait
    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deadline -and (Test-Path $appExe)) { Start-Sleep -Milliseconds 300 }
    Check (-not (Test-Path $appExe)) '程序文件已删除'
    Check (-not (Test-Path $shortcut)) '开始菜单快捷方式已删除'
    Check ($null -eq (Debugger)) '接管登记已撤销'
    Check ($null -eq (AutostartValue 'Run')) '开机启动登记已删除'
    Check ($null -eq (AutostartValue 'Approved')) 'StartupApproved 值已删除'
}
finally {
    if ($instance) { Stop-Process -Id $instance.Id -Force -ErrorAction SilentlyContinue }
    # 中途失败时卸载程序可能没跑，补删测试残留。
    if (Test-Path (Join-Path $appDir 'unins000.exe')) {
        Start-Process -FilePath (Join-Path $appDir 'unins000.exe') -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES' -Wait
        Start-Sleep -Seconds 2
    }
    Remove-Item -Force $shortcut -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force "HKCU:\$testBase", "HKCU:\$autoBase" -ErrorAction SilentlyContinue
    if ($failures.Count) { "日志保留在 $tmp" } else { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue }
}

if ($failures.Count) {
    "失败 $($failures.Count) 项："
    $failures | ForEach-Object { "  - $_" }
    exit 1
}
"全部通过"
