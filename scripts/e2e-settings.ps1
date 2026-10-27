# 设置持久化端到端验收：用临时 TML_SETTINGS_DIR 写入非默认设置，启动 exe，检查窗口位置/大小被应用，
# 再用 WM_CLOSE 优雅关闭，检查设置文件被写回；另测损坏文件、显示器之外的位置、开发模式不碰设置文件。
# 只结束自己启动的进程。用法：pwsh scripts/e2e-settings.ps1 [-Exe target/release/TaskManagerLite.exe]
param([string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe")

$ErrorActionPreference = 'Stop'
$exePath = (Resolve-Path $Exe).Path
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class Win {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
}
'@
[void][Win]::SetProcessDPIAware()

$root = Join-Path ([System.IO.Path]::GetTempPath()) "tml-e2e-settings-$(Get-Random)"
$logDir = Join-Path $root 'logs'
New-Item -ItemType Directory -Force $logDir | Out-Null
$env:TML_LOG_DIR = $logDir
$failures = New-Object System.Collections.Generic.List[string]

function Check($ok, $msg) {
    if ($ok) { "  通过：$msg" } else { "  失败：$msg"; $script:failures.Add($msg) }
}

# 启动 exe，等主窗口出现；返回 @{Process; Handle}。
function Start-App($settingsDir, $extraArgs = @()) {
    $env:TML_SETTINGS_DIR = $settingsDir
    $p = Start-Process -FilePath $exePath -ArgumentList $extraArgs -PassThru
    $h = [IntPtr]::Zero
    for ($i = 0; $i -lt 150 -and $h -eq [IntPtr]::Zero; $i++) {
        Start-Sleep -Milliseconds 100
        $p.Refresh()
        if ($p.HasExited) { throw "进程提前退出，退出码 $($p.ExitCode)" }
        $h = $p.MainWindowHandle
    }
    if ($h -eq [IntPtr]::Zero) { $p.Kill(); throw '等不到主窗口' }
    # 再等一会儿让窗口完成首次布局与位置设置。
    Start-Sleep -Milliseconds 800
    @{ Process = $p; Handle = $h }
}

function Close-App($app) {
    [void][Win]::PostMessage($app.Handle, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) # WM_CLOSE
    if (-not $app.Process.WaitForExit(10000)) { $app.Process.Kill(); throw '收到 WM_CLOSE 后 10 秒内未退出' }
}

function Window-State($app) {
    $r = New-Object Win+RECT; $c = New-Object Win+RECT
    [void][Win]::GetWindowRect($app.Handle, [ref]$r)
    [void][Win]::GetClientRect($app.Handle, [ref]$c)
    @{ X = $r.Left; Y = $r.Top; W = $c.Right; H = $c.Bottom }
}

function Scale($app) { [Win]::GetDpiForWindow($app.Handle) / 96.0 }

# 客户区物理大小应约等于逻辑尺寸 × 缩放（允许 2 像素的取整误差）。
function Near-Size($app, $w, $logicalW, $logicalH) {
    $s = Scale $app
    [math]::Abs($w.W - $logicalW * $s) -le 2 -and [math]::Abs($w.H - $logicalH * $s) -le 2
}

function New-Dir($name) { $d = Join-Path $root $name; New-Item -ItemType Directory -Force $d | Out-Null; $d }

try {
    '== 1. 非默认设置被应用，退出时写回 =='
    $dir = New-Dir 'main'
    $settingsFile = Join-Path $dir 'settings.json'
    @'
{
  "window": { "x": 120, "y": 90, "width": 800, "height": 560, "maximized": false },
  "interval_index": 2,
  "tree": true,
  "sort": { "key": "memory", "descending": false },
  "page": 1,
  "resource": "network",
  "columns": { "pid": 90, "cpu": 64, "memory": 140, "disk": 200 },
  "future_field": { "keep": [1, 2, 3] }
}
'@ | Set-Content -Encoding utf8 $settingsFile
    $app = Start-App $dir
    $w = Window-State $app
    Check ($w.X -eq 120 -and $w.Y -eq 90) "窗口位置 = (120, 90)，实际 ($($w.X), $($w.Y))"
    Check (Near-Size $app $w 800 560) "客户区大小对应逻辑尺寸 800x560，实际 $($w.W)x$($w.H)（缩放 $(Scale $app)）"
    # 等到至少一个快照帧之后，确认恢复的性能页选中项（network）没有被首个快照重置成 cpu。
    Start-Sleep -Milliseconds 2500
    Close-App $app
    $log = Get-ChildItem $logDir -Filter *.log | Sort-Object LastWriteTime | Select-Object -Last 1 | Get-Content -Raw
    Check ($log -match '设置 Some\(Loaded\)') '日志显示设置已读取'
    Check ($log -notmatch '设置中的资源') '性能页资源 id 在启动时恢复成功'
    $s = Get-Content $settingsFile -Raw | ConvertFrom-Json
    Check ($s.window.x -eq 120 -and $s.window.y -eq 90 -and [math]::Abs($s.window.width - 800) -lt 1.5 -and [math]::Abs($s.window.height - 560) -lt 1.5) "写回的窗口位置与逻辑大小一致（$($s.window.width)x$($s.window.height)）"
    Check ($s.interval_index -eq 2) '采样间隔 = 2'
    Check ($s.tree -eq $true) '树形 = true'
    Check ($s.sort.key -eq 'memory' -and $s.sort.descending -eq $false) '排序 = 内存升序'
    Check ($s.page -eq 1) '页面 = 性能'
    Check ($s.resource -eq 'network') '首个快照之后性能页选中的资源仍是 network（退出时从选中项读取）'
    Check ($s.columns.pid -eq 90 -and $s.columns.cpu -eq 64 -and $s.columns.memory -eq 140 -and $s.columns.disk -eq 200) '列宽一致'
    Check ($s.future_field.keep.Count -eq 3) '未知字段被保留'
    Check (-not (Test-Path "$settingsFile.tmp")) '没有残留临时文件'

    '== 2. 窗口被移动后退出，新位置被保存 =='
    $app = Start-App $dir
    # SWP_NOZORDER | SWP_NOACTIVATE，保持大小不变，只移动位置。
    [void][Win]::SetWindowPos($app.Handle, [IntPtr]::Zero, 300, 200, 0, 0, 0x0001 -bor 0x0004 -bor 0x0010)
    Start-Sleep -Milliseconds 300
    Close-App $app
    $win = (Get-Content $settingsFile -Raw | ConvertFrom-Json).window
    Check ($win.x -eq 300 -and $win.y -eq 200) "移动后的位置 (300, 200) 被保存，实际 ($($win.x), $($win.y))"
    Check ([math]::Abs($win.width - 800) -lt 1.5 -and [math]::Abs($win.height - 560) -lt 1.5) "逻辑大小保持 800x560，实际 $($win.width)x$($win.height)"

    '== 3. 损坏的设置文件：正常启动并重写 =='
    $dir = New-Dir 'corrupt'
    $settingsFile = Join-Path $dir 'settings.json'
    '{ "window": { "x": 1, "y": ' | Set-Content -Encoding utf8 $settingsFile
    $app = Start-App $dir
    Check (-not $app.Process.HasExited) '损坏的文件不影响启动'
    Close-App $app
    $s = Get-Content $settingsFile -Raw | ConvertFrom-Json
    Check ($s.interval_index -eq 1 -and $s.sort.key -eq 'cpu' -and $s.page -eq 0) '退出后已写回合法的默认设置'
    '== 4. 缺失目录：首次启动创建设置文件 =='
    $dir = Join-Path $root 'fresh\nested'
    $app = Start-App $dir
    Close-App $app
    Check (Test-Path (Join-Path $dir 'settings.json')) '设置目录与文件被自动创建'

    '== 5. 保存的位置不在任何显示器内：不恢复位置，但恢复大小 =='
    $dir = New-Dir 'offscreen'
    '{ "window": { "x": -30000, "y": -30000, "width": 820, "height": 580 } }' | Set-Content -Encoding utf8 (Join-Path $dir 'settings.json')
    $app = Start-App $dir
    $w = Window-State $app
    Check ($w.X -gt -10000 -and $w.Y -gt -10000) "窗口位置回到可见区域，实际 ($($w.X), $($w.Y))"
    Check (Near-Size $app $w 820 580) "客户区大小对应逻辑尺寸 820x580，实际 $($w.W)x$($w.H)"
    Close-App $app

    '== 6. 开发模式（--measure-startup）不读写设置文件 =='
    $dir = New-Dir 'dev'
    $settingsFile = Join-Path $dir 'settings.json'
    '{ "interval_index": 3, "page": 1 }' | Set-Content -Encoding utf8 $settingsFile
    $hashBefore = (Get-FileHash $settingsFile).Hash
    $env:TML_SETTINGS_DIR = $dir
    $p = Start-Process -FilePath $exePath -ArgumentList '--measure-startup' -PassThru
    if (-not $p.WaitForExit(20000)) { $p.Kill(); throw '--measure-startup 超时' }
    Check ((Get-FileHash $settingsFile).Hash -eq $hashBefore) '设置文件内容未被改动'
    $files = @(Get-ChildItem $dir)
    Check ($files.Count -eq 1) '设置目录里没有新文件'
}
finally {
    Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}

if ($failures.Count -gt 0) { "`n失败 $($failures.Count) 项"; exit 1 }
"`n全部通过"
