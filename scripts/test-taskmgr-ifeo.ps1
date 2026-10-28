# 接管 Ctrl+Shift+Esc 的命令行开关端到端验收（见 docs/adr/0003-replace-taskmgr-via-ifeo.md）。
# 通过环境变量 TML_IFEO_TEST_KEY 把登记改写到 HKCU\Software\TaskManagerLite-test\<随机名>\Taskmgr.exe，
# 全程不提权、不弹 UAC、不碰 HKLM（真实的 Taskmgr.exe IFEO 键）；结束时删除测试键。
# 覆盖：状态/登记/撤销的退出码、写入值带引号、路径失效与被占用、非法测试键被拒绝、被 IFEO 启动时忽略 taskmgr 参数。
# 用法：pwsh scripts/test-taskmgr-ifeo.ps1 [-Exe target/release/TaskManagerLite.exe]
param([string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe")

$ErrorActionPreference = 'Stop'
$exePath = (Resolve-Path $Exe).Path
$name = "e2e-$PID-$(Get-Random)"
$testKey = "Software\TaskManagerLite-test\$name\Taskmgr.exe"
$psKey = "HKCU:\$testKey"
$failures = New-Object System.Collections.Generic.List[string]
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) "tml-ifeo-$name"
New-Item -ItemType Directory -Force $tmp | Out-Null
# 登记前会检查 exe 及目录是否可被 Everyone/Users 写入；仓库构建目录的 ACL 因机器而异，
# 所以改用用户临时目录（继承用户 profile 的 ACL，默认安全）里的副本做被测程序。
$dirA = Join-Path $tmp 'a'
New-Item -ItemType Directory -Force $dirA | Out-Null
Copy-Item $exePath (Join-Path $dirA 'TaskManagerLite.exe')
$exePath = Join-Path $dirA 'TaskManagerLite.exe'

function Check($ok, $msg) {
    if ($ok) { "  通过：$msg" } else { "  失败：$msg"; $script:failures.Add($msg) }
}

# 运行 exe 的开关并返回退出码（界面子系统程序，必须 -Wait 才拿得到退出码）。
function Run($exe, [string[]]$argList) {
    $p = Start-Process -FilePath $exe -ArgumentList $argList -PassThru -Wait -WindowStyle Hidden
    $p.ExitCode
}

function Debugger { (Get-ItemProperty -Path $psKey -Name Debugger -ErrorAction SilentlyContinue).Debugger }

try {
    $env:TML_IFEO_TEST_KEY = $testKey

    "== 初始：未登记"
    Check ((Run $exePath @('--taskmgr-status')) -eq 10) '状态查询返回 10（未登记）'
    Check (-not (Test-Path $psKey)) '查询不会创建键'

    "== 登记"
    Check ((Run $exePath @('--register-taskmgr')) -eq 0) '登记返回 0'
    Check ((Debugger) -eq "`"$exePath`"") "Debugger 为带引号的 exe 路径：$(Debugger)"
    Check ((Run $exePath @('--taskmgr-status')) -eq 0) '状态查询返回 0（指向本 exe）'
    Check ((Run $exePath @('--register-taskmgr')) -eq 0) '重复登记幂等'

    "== 另一份 exe（路径不同）遇到已有的有效登记"
    $copy = Join-Path $tmp 'Task Manager Lite.exe'
    Copy-Item $exePath $copy
    Check ((Run $copy @('--taskmgr-status')) -eq 11) '副本查询返回 11（指向其他有效程序）'
    Check ((Run $copy @('--register-taskmgr')) -eq 3) '副本不加 --force 登记被拒绝（3）'
    Check ((Debugger) -eq "`"$exePath`"") '被拒绝时原值不变'
    Check ((Run $copy @('--unregister-taskmgr')) -eq 3) '副本不加 --force 撤销也被拒绝（3）'
    Check ((Run $copy @('--register-taskmgr', '--force')) -eq 0) '加 --force 登记成功'
    Check ((Debugger) -eq "`"$copy`"") "含空格的路径被正确加引号：$(Debugger)"
    Check ((Run $copy @('--taskmgr-status')) -eq 0) '副本查询返回 0'

    "== exe 被移动/删除后登记失效"
    Remove-Item $copy -Force
    Check ((Run $exePath @('--taskmgr-status')) -eq 12) '原 exe 查询返回 12（登记的路径已失效）'
    Check ((Run $exePath @('--register-taskmgr')) -eq 0) '失效的登记不算占用，可直接重新登记'
    Check ((Debugger) -eq "`"$exePath`"") '重新登记到当前位置'

    "== 撤销"
    Check ((Run $exePath @('--unregister-taskmgr')) -eq 0) '撤销返回 0'
    Check (-not (Test-Path $psKey)) 'Debugger 删除后空键一并删除'
    Check ((Run $exePath @('--taskmgr-status')) -eq 10) '撤销后状态为未登记'
    Check ((Run $exePath @('--unregister-taskmgr')) -eq 0) '重复撤销是空操作'

    "== 位置检查：exe 所在目录可被 Users 写入时拒绝登记（--force 绕过）"
    $dirC = Join-Path $tmp 'c'
    New-Item -ItemType Directory -Force $dirC | Out-Null
    $exeC = Join-Path $dirC 'TaskManagerLite.exe'
    Copy-Item $exePath $exeC
    Check ((Run $exeC @('--register-taskmgr')) -eq 0) '默认安全的目录可以登记'
    Check ((Run $exeC @('--unregister-taskmgr')) -eq 0) '撤销'
    & icacls $dirC /grant '*S-1-5-32-545:M' | Out-Null
    Check ($LASTEXITCODE -eq 0) '给目录授予 BUILTIN\Users 修改权限（仅用于测试）'
    Check ((Run $exeC @('--register-taskmgr')) -eq 5) '目录可被 Users 写入：登记被拒绝（5）'
    Check (-not (Test-Path $psKey)) '被拒绝时没有写注册表'
    Check ((Run $exeC @('--register-taskmgr', '--force')) -eq 0) '加 --force 可绕过位置检查'
    Check ((Run $exeC @('--taskmgr-status')) -eq 0) '强制登记后状态为已登记'
    Check ((Run $exeC @('--unregister-taskmgr', '--elevated-child')) -eq 0) '带 --elevated-child 的撤销照常工作（子进程模式）'
    Check (-not (Test-Path $psKey)) '撤销后键已清理'

    "== 非法测试键被拒绝，不会退回真实键"
    $env:TML_IFEO_TEST_KEY = 'SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\Taskmgr.exe'
    Check ((Run $exePath @('--register-taskmgr')) -eq 1) '不以测试前缀开头的键被拒绝（退出码 1）'
    $env:TML_IFEO_TEST_KEY = 'Software\TaskManagerLite-test\'
    Check ((Run $exePath @('--register-taskmgr')) -eq 1) '只有前缀、没有名字的键被拒绝（退出码 1）'
    $env:TML_IFEO_TEST_KEY = $testKey

    "== 被 IFEO 重定向启动：第一个参数是 taskmgr.exe 路径，应被忽略且正常启动"
    $logDir = Join-Path $tmp 'logs'
    New-Item -ItemType Directory -Force $logDir | Out-Null
    $env:TML_LOG_DIR = $logDir
    $env:TML_SETTINGS_DIR = Join-Path $tmp 'settings'
    # --measure-startup 在首个快照帧后自动退出，且不受单实例限制，不会干扰用户已开的实例。
    $code = Run $exePath @('C:\WINDOWS\system32\taskmgr.exe', '/0', '--measure-startup')
    Check ($code -eq 0) "带 taskmgr 参数启动并正常退出（退出码 $code）"
    $log = Get-ChildItem $logDir -Filter *.log -ErrorAction SilentlyContinue | Select-Object -First 1
    $text = if ($log) { Get-Content $log.FullName -Raw -Encoding UTF8 } else { '' }
    Check ($text -match 'IFEO') '日志记录了由 IFEO 重定向启动'
    Check ($text -match '首个快照帧') '界面正常出了首个快照帧'
}
finally {
    Remove-Item Env:TML_IFEO_TEST_KEY, Env:TML_LOG_DIR, Env:TML_SETTINGS_DIR -ErrorAction SilentlyContinue
    # 只删本脚本的测试键；父键 TaskManagerLite-test 为空时一并删除。
    Remove-Item -Path "HKCU:\Software\TaskManagerLite-test\$name" -Recurse -Force -ErrorAction SilentlyContinue
    if (-not (Get-ChildItem 'HKCU:\Software\TaskManagerLite-test' -ErrorAction SilentlyContinue)) {
        Remove-Item -Path 'HKCU:\Software\TaskManagerLite-test' -Force -ErrorAction SilentlyContinue
    }
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

if ($failures.Count -gt 0) {
    "`n失败 $($failures.Count) 项："
    $failures | ForEach-Object { "  - $_" }
    exit 1
}
"`n全部通过"
