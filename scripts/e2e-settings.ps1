# 设置持久化端到端验收：用临时 TML_SETTINGS_DIR 写入非默认设置，启动 exe，检查窗口位置/大小被应用，
# 再用 WM_CLOSE 优雅关闭，检查设置文件被写回；另测损坏文件、显示器之外的位置、窗口置顶、详细信息列、迷你模式、开发模式不碰设置文件。
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
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr h);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int index);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetLayeredWindowAttributes(IntPtr h, out uint key, out byte alpha, out uint flags);
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
    # --allow-multiple：不受单实例限制，用户自己开着的任务管理器不会干扰测试。
    $p = Start-Process -FilePath $exePath -ArgumentList (@("--allow-multiple") + $extraArgs) -PassThru
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
    # 句柄可能先指向尚未定型的窗口：重新取一次，并等到它有合理的大小。
    for ($i = 0; $i -lt 50; $i++) {
        $p.Refresh()
        if ($p.HasExited) { throw "进程提前退出，退出码 $($p.ExitCode)" }
        $h = $p.MainWindowHandle
        $r = New-Object Win+RECT
        if ($h -ne [IntPtr]::Zero -and [Win]::GetWindowRect($h, [ref]$r) -and ($r.Right - $r.Left) -gt 200) { break }
        Start-Sleep -Milliseconds 100
    }
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
    Check ($s.view -eq 'tree' -and $null -eq $s.tree) '旧版 tree: true 迁移为 view = tree，且不再写出 tree'
    Check ($s.sort.key -eq 'memory' -and $s.sort.descending -eq $false) '排序 = 内存升序'
    Check ($s.page -eq 1) '页面 = 性能'
    Check ($s.resource -eq 'network') '首个快照之后性能页选中的资源仍是 network（退出时从选中项读取）'
    Check ($s.columns.pid -eq 90 -and $s.columns.cpu -eq 64 -and $s.columns.memory -eq 140 -and $s.columns.disk -eq 200) '列宽一致'
    Check ($s.future_field.keep.Count -eq 3) '未知字段被保留'
    Check (@(Get-ChildItem $dir -Filter *.tmp).Count -eq 0) "没有残留临时文件"

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

    '== 5b. 保存时是最大化：恢复为最大化，且保存的仍是普通矩形 =='
    $dir = New-Dir 'maximized'
    $settingsFile = Join-Path $dir 'settings.json'
    '{ "window": { "x": 150, "y": 100, "width": 800, "height": 560, "maximized": true } }' | Set-Content -Encoding utf8 $settingsFile
    $app = Start-App $dir
    Check ([Win]::IsZoomed($app.Handle)) '设置为最大化时启动即最大化'
    Close-App $app
    $win = (Get-Content $settingsFile -Raw | ConvertFrom-Json).window
    Check ($win.maximized -eq $true) '最大化状态被保存'
    Check ($win.x -eq 150 -and $win.y -eq 100 -and [math]::Abs($win.width - 800) -lt 1.5 -and [math]::Abs($win.height - 560) -lt 1.5) "保存的仍是普通矩形 (150,100) 800x560，实际 ($($win.x),$($win.y)) $($win.width)x$($win.height)"

    '== 5c. 运行中最大化再退出：保存最大化之前的普通矩形 =='
    $dir = New-Dir 'maximize-runtime'
    $settingsFile = Join-Path $dir 'settings.json'
    '{ "window": { "x": 200, "y": 120, "width": 800, "height": 560 } }' | Set-Content -Encoding utf8 $settingsFile
    $app = Start-App $dir
    [void][Win]::ShowWindow($app.Handle, 3) # SW_MAXIMIZE
    Start-Sleep -Milliseconds 800
    Check ([Win]::IsZoomed($app.Handle)) '窗口已最大化'
    Close-App $app
    $win = (Get-Content $settingsFile -Raw | ConvertFrom-Json).window
    Check ($win.maximized -eq $true) '最大化状态被保存'
    Check ($win.x -eq 200 -and $win.y -eq 120 -and [math]::Abs($win.width - 800) -lt 1.5 -and [math]::Abs($win.height - 560) -lt 1.5) "保存的是最大化之前的矩形，实际 ($($win.x),$($win.y)) $($win.width)x$($win.height)"

    '== 5d. 首次运行（无设置）就最大化再退出：不能把整屏矩形当作普通矩形 =='
    $dir = New-Dir 'first-maximize'
    $settingsFile = Join-Path $dir 'settings.json'
    $app = Start-App $dir
    [void][Win]::ShowWindow($app.Handle, 3)
    Start-Sleep -Milliseconds 800
    Close-App $app
    $win = (Get-Content $settingsFile -Raw | ConvertFrom-Json).window
    Check ($win.maximized -eq $true) '最大化状态被保存'
    Check ([math]::Abs($win.width - 1040) -lt 1.5 -and [math]::Abs($win.height - 700) -lt 1.5) "保存的是默认窗口大小 1040x700，实际 $($win.width)x$($win.height)"

    '== 5e. 保存的资源 id 暂不存在：保留它而不是回退的 cpu =='
    $dir = New-Dir 'pending'
    $settingsFile = Join-Path $dir 'settings.json'
    '{ "page": 1, "resource": "gpu:0" }' | Set-Content -Encoding utf8 $settingsFile
    $app = Start-App $dir
    Start-Sleep -Milliseconds 1500
    Close-App $app
    $log = Get-ChildItem $logDir -Filter *.log | Sort-Object LastWriteTime | Select-Object -Last 1 | Get-Content -Raw
    Check ($log -match '暂不存在') '日志记录了待恢复的资源'
    Check ((Get-Content $settingsFile -Raw | ConvertFrom-Json).resource -eq 'gpu:0') '退出时仍保存 gpu:0'

    '== 5f. 异常巨大的设置文件：视为损坏，正常启动 =='
    $dir = New-Dir 'huge'
    $settingsFile = Join-Path $dir 'settings.json'
    '{ "tree": true, "pad": "' + ('x' * (2 * 1024 * 1024)) + '" }' | Set-Content -Encoding utf8 $settingsFile
    $app = Start-App $dir
    Check (-not $app.Process.HasExited) '超大文件不影响启动'
    Close-App $app
    Check ((Get-Item $settingsFile).Length -lt 4096) '退出后已写回正常大小的设置'
    Check (@(Get-ChildItem $dir -Filter *.tmp).Count -eq 0) '没有残留临时文件'

    '== 5g. 窗口置顶：按设置恢复，点击图钉切换，退出时写回 =='
    $dir = New-Dir 'pin'
    $settingsFile = Join-Path $dir 'settings.json'
    # 进程页、800 宽：工具栏处于最窄档，提权按钮（未提权时）收成 32px 图标，图钉在它左侧。
    '{ "window": { "x": 140, "y": 110, "width": 800, "height": 560, "maximized": false }, "page": 0, "always_on_top": true }' | Set-Content -Encoding utf8 $settingsFile
    $topmost = { param($a) ([Win]::GetWindowLong($a.Handle, -20) -band 0x8) -ne 0 } # GWL_EXSTYLE, WS_EX_TOPMOST
    $app = Start-App $dir
    Check (& $topmost $app) '设置 always_on_top = true 时窗口置顶'
    $elevated = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    $s = Scale $app
    $w = Window-State $app
    $px = [int]($w.W - ($(if ($elevated) { 12 + 16 } else { 12 + 32 + 8 + 16 }) * $s))
    $py = [int](28 * $s)
    $lp = [IntPtr](($py -shl 16) -bor $px)
    [void][Win]::PostMessage($app.Handle, 0x0200, [IntPtr]::Zero, $lp) # WM_MOUSEMOVE
    [void][Win]::PostMessage($app.Handle, 0x0201, [IntPtr]1, $lp)      # WM_LBUTTONDOWN
    [void][Win]::PostMessage($app.Handle, 0x0202, [IntPtr]::Zero, $lp) # WM_LBUTTONUP
    Start-Sleep -Milliseconds 500
    Check (-not (& $topmost $app)) '点击图钉后取消置顶'
    Close-App $app
    Check ((Get-Content $settingsFile -Raw | ConvertFrom-Json).always_on_top -eq $false) '退出时写回 always_on_top = false'
    $app = Start-App $dir
    Check (-not (& $topmost $app)) '再次启动时不置顶'
    Close-App $app

    '== 5h. 详细信息列：顺序、宽度与按详细信息列排序被恢复并写回；排序列未显示时回到默认排序 =='
    $dir = New-Dir 'details'
    $settingsFile = Join-Path $dir 'settings.json'
    '{ "detail_columns": [ { "id": "user", "width": 150 }, { "id": "threads", "width": 72 }, { "id": "command_line", "width": 420.5 } ], "sort": { "key": "detail:threads", "descending": true } }' | Set-Content -Encoding utf8 $settingsFile
    $app = Start-App $dir
    Start-Sleep -Milliseconds 1500
    Check (-not $app.Process.HasExited) '显示详细信息列时正常运行'
    Close-App $app
    $s = Get-Content $settingsFile -Raw | ConvertFrom-Json
    $cols = @($s.detail_columns | ForEach-Object { "$($_.id)=$($_.width)" }) -join ','
    Check ($cols -eq 'user=150,threads=72,command_line=420.5') "详细信息列的顺序与宽度被写回，实际 $cols"
    Check ($s.sort.key -eq 'detail:threads' -and $s.sort.descending -eq $true) "按线程降序被写回，实际 $($s.sort.key) $($s.sort.descending)"
    '{ "detail_columns": [ { "id": "threads", "width": 72 } ], "sort": { "key": "detail:user", "descending": false } }' | Set-Content -Encoding utf8 $settingsFile
    $app = Start-App $dir
    Close-App $app
    $s = Get-Content $settingsFile -Raw | ConvertFrom-Json
    Check ($s.sort.key -eq 'cpu' -and $s.sort.descending -eq $true) "排序列（用户名）未显示时回到 CPU 降序，实际 $($s.sort.key) $($s.sort.descending)"

    '== 5i. 迷你模式：按设置以迷你窗口启动，位置、置顶、不透明度被应用；移动后保存；双击回到完整模式 =='
    $dir = New-Dir 'mini'
    $settingsFile = Join-Path $dir 'settings.json'
    '{ "window": { "x": 140, "y": 110, "width": 800, "height": 560 }, "page": 1, "mini": { "active": true, "x": 300, "y": 200, "on_top": true, "opacity": 80 } }' | Set-Content -Encoding utf8 $settingsFile
    $app = Start-App $dir
    $w = Window-State $app
    $s = Scale $app
    Check ($w.X -eq 300 -and $w.Y -eq 200) "迷你窗口位置 = (300, 200)，实际 ($($w.X), $($w.Y))"
    Check ([math]::Abs($w.W - 250 * $s) -le 2) "迷你窗口宽 250（逻辑），实际 $($w.W) 物理像素（缩放 $s）"
    Check (& $topmost $app) '迷你窗口置顶'
    $key = 0; $alpha = 0; $flags = 0
    $layered = [Win]::GetLayeredWindowAttributes($app.Handle, [ref]$key, [ref]$alpha, [ref]$flags)
    Check ($layered -and $alpha -eq 204) "不透明度 80%（alpha 204），实际 $(if ($layered) { $alpha } else { '未分层' })"
    [void][Win]::SetWindowPos($app.Handle, [IntPtr]::Zero, 420, 260, 0, 0, 0x0001 -bor 0x0004 -bor 0x0010)
    Start-Sleep -Milliseconds 600
    Close-App $app
    $s2 = Get-Content $settingsFile -Raw | ConvertFrom-Json
    Check ($s2.mini.active -eq $true -and $s2.mini.x -eq 420 -and $s2.mini.y -eq 260) "退出时仍在迷你模式，新位置 (420, 260) 被保存，实际 $($s2.mini.active) ($($s2.mini.x), $($s2.mini.y))"
    Check ($s2.mini.opacity -eq 80 -and $s2.mini.on_top -eq $true) '不透明度与置顶被写回'
    Check ($s2.window.x -eq 140 -and $s2.window.y -eq 110 -and [math]::Abs($s2.window.width - 800) -lt 1.5) "完整窗口的矩形不受迷你模式影响，实际 ($($s2.window.x),$($s2.window.y)) $($s2.window.width)"
    $app = Start-App $dir
    $w = Window-State $app
    Check ($w.X -eq 420 -and $w.Y -eq 260) "再次启动时迷你窗口在 (420, 260)，实际 ($($w.X), $($w.Y))"
    # 在第一张卡片上双击（两次按下松开）。
    $lp = [IntPtr]((([int](30 * $s)) -shl 16) -bor [int](120 * $s))
    [void][Win]::PostMessage($app.Handle, 0x0200, [IntPtr]::Zero, $lp)
    foreach ($i in 1..2) {
        [void][Win]::PostMessage($app.Handle, 0x0201, [IntPtr]1, $lp)
        [void][Win]::PostMessage($app.Handle, 0x0202, [IntPtr]::Zero, $lp)
    }
    Start-Sleep -Milliseconds 800
    Check (-not [Win]::IsWindowVisible($app.Handle)) '双击后迷你窗口隐藏'
    $mini = $app.Handle
    $app.Process.Refresh()
    $app.Handle = $app.Process.MainWindowHandle
    $w = Window-State $app
    Check ($app.Handle -ne $mini -and $w.X -eq 140 -and $w.Y -eq 110) "完整窗口出现在 (140, 110)，实际 ($($w.X), $($w.Y))"
    Close-App $app
    $s2 = Get-Content $settingsFile -Raw | ConvertFrom-Json
    Check ($s2.mini.active -eq $false -and $s2.mini.x -eq 420) "回到完整模式后退出：mini.active = false，迷你位置保留，实际 $($s2.mini.active) $($s2.mini.x)"
    Check ($s2.page -eq 1) '页签仍是性能页'

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
