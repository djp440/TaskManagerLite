# 性能曲线悬停提示（#81）端到端验收：不依赖 SendInput，用临时设置目录（TML_SETTINGS_DIR）与日志目录启动自己的实例（--allow-multiple）。
#   1. 钩子 --hover：启动后第 N 秒把性能页切到各曲线、把鼠标“放”到指定位置，读回提示写进日志的数值 / 距今 / 进程 PID。
#      脚本先启动自己的高优先级忙循环子进程（几秒后自行结束并被清理）造一次 CPU 尖峰，再核对：
#      - 60 秒与 5 分钟窗口里，悬停在曲线上记着这个忙循环进程的点（`cpu:pid=<PID>`）时，提示的进程 PID 就是它、数值是尖峰；
#      - 提示数值与曲线高度一致（圆点纵坐标 = 图高 × (1 - 数值 / 量程)）；最右侧的点距现在不超过一个采样间隔；
#      - 曲线最左（尚无数据）是“无数据”；内存、网络曲线没有进程（网络没有进程级数据）；日志里没有进程名。
#   2. 真实鼠标事件路径：向主窗口 PostMessage WM_MOUSEMOVE（不动用户的鼠标、不用 SendInput），日志出现“悬停 进入”，再 WM_MOUSELEAVE 出现“悬停 离开”。
# 只结束自己启动的进程。用法：pwsh scripts/test-hover.ps1 [-Exe target/release/TaskManagerLite.exe] [-HookSeconds 14]
param(
    [int]$HookSeconds = 14,
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe"
)

$ErrorActionPreference = 'Stop'
$exePath = (Resolve-Path $Exe).Path
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class HoverWin {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
}
'@

$root = Join-Path ([System.IO.Path]::GetTempPath()) "tml-hover-$(Get-Random)"
New-Item -ItemType Directory -Force $root | Out-Null
$failures = New-Object System.Collections.Generic.List[string]
$procs = New-Object System.Collections.Generic.List[object]
function Check($ok, $msg) {
    if ($ok) { "  通过：$msg" } else { "  失败：$msg"; $script:failures.Add($msg) }
}
function Num($line, $key) { if ($line -match "$key=(-?\d+(\.\d+)?)") { [double]$Matches[1] } else { [double]::NaN } }
function Word($line, $key) { if ($line -match "$key=(\S+)") { $Matches[1] } else { '' } }

function Start-App($name, $json, $extraArgs = @()) {
    $settingsDir = Join-Path $root "settings-$name"
    $logDir = Join-Path $root "logs-$name"
    New-Item -ItemType Directory -Force $settingsDir, $logDir | Out-Null
    Set-Content -Path (Join-Path $settingsDir 'settings.json') -Value $json -Encoding utf8
    $env:TML_SETTINGS_DIR = $settingsDir
    $env:TML_LOG_DIR = $logDir
    $p = Start-Process -FilePath $exePath -ArgumentList (@('--allow-multiple') + $extraArgs) -PassThru
    $script:procs.Add($p)
    $h = [IntPtr]::Zero
    for ($i = 0; $i -lt 150 -and $h -eq [IntPtr]::Zero; $i++) {
        Start-Sleep -Milliseconds 100
        $p.Refresh()
        if ($p.HasExited) { throw "进程提前退出，退出码 $($p.ExitCode)" }
        $h = $p.MainWindowHandle
    }
    if ($h -eq [IntPtr]::Zero) { throw '等不到主窗口' }
    @{ Process = $p; Handle = $h; LogDir = $logDir }
}

function Close-App($app) {
    [void][HoverWin]::PostMessage($app.Handle, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) # WM_CLOSE
    if (-not $app.Process.WaitForExit(20000)) { $app.Process.Kill(); throw '收到 WM_CLOSE 后 20 秒内未退出' }
}

function Log-Lines($app) {
    $log = Get-ChildItem $app.LogDir -Filter *.log | Sort-Object LastWriteTime | Select-Object -Last 1
    (Get-Content $log.FullName -Raw -Encoding utf8) -split "`r?`n"
}

# 忙循环子进程：所有逻辑处理器各一个线程自旋 $Seconds 秒，高优先级，保证它是这几秒里占用 CPU 最高的进程。
function Start-Burner($seconds) {
    # 自旋放在编译出来的 C# 里（脚本块不能在线程池线程上执行）；Add-Type 编译约 1 秒，之后才开始自旋。
    $code = 'Add-Type ''public static class Burn { public static void Spin(int s) { var sw = System.Diagnostics.Stopwatch.StartNew(); System.Threading.Tasks.Parallel.For(0, System.Environment.ProcessorCount * 2, i => { while (sw.Elapsed.TotalSeconds < s) { } }); } }''; [Burn]::Spin(' + $seconds + ')'
    $enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code))
    $b = Start-Process -FilePath (Get-Process -Id $PID).Path -ArgumentList '-NoProfile', '-EncodedCommand', $enc -PassThru -WindowStyle Hidden
    $script:procs.Add($b)
    try { $b.PriorityClass = 'High' } catch { }
    $b
}

# 提示数值与曲线高度一致：圆点纵坐标 = 图高 × (1 - 数值 / 量程)。
function Check-Height($line, $what) {
    $v = Num $line '数值'; $scale = Num $line '量程'; $h = Num $line '图高'; $y = Num $line '圆点y'
    $want = $h * (1 - [math]::Min(1.0, [math]::Max(0.0, $v / $scale)))
    Check ([math]::Abs($y - $want) -lt 0.6) "$what：圆点高度 $y 与数值 $v / 量程 $scale 对应的 $([math]::Round($want, 2)) 一致"
}

function Run-Hook($windowKey) {
    # 忙循环先启动（PID 要写进钩子参数），应用随后启动，忙循环自旋的几秒落在曲线最右侧约 3～12 秒前。
    $burner = Start-Burner 7
    "  忙循环子进程 PID $($burner.Id)（编译后自旋 7 秒，随后自行结束）"
    $spec = "cpu:peak,cpu:pid=$($burner.Id),cpu:1,cpu:0,memory:1,disk:peak,network:1,gpu:1"
    $app = Start-App "hook-$windowKey" "{`"history_window`": `"$windowKey`", `"page`": 1}" @('--hover', "$spec@$HookSeconds")
    Start-Sleep -Seconds ($HookSeconds + 3)
    # 先结束忙循环（它是高优先级、占满所有逻辑处理器，留着会拖慢应用响应 WM_CLOSE），再读日志、关应用。
    if (-not $burner.HasExited) { $burner.Kill(); $burner.WaitForExit(5000) | Out-Null }
    $lines = Log-Lines $app
    Close-App $app
    @{ Burner = $burner; Lines = $lines }
}

try {
    foreach ($key in '60s', '5m') {
        "== 1. 窗口 $key：CPU 尖峰处悬停 =="
        # 本机若有别的进程同时满载，聚合桶里并列的最大值以先到的采样为准（见 History 的并列规则），忙循环那几个桶可能一个都没记到它：最多重试一次。
        for ($attempt = 1; $attempt -le 2; $attempt++) {
            $r = Run-Hook $key
            $hook = $r.Lines | Where-Object { $_ -match '悬停 钩子' }
            $spikeLine = @($hook | Where-Object { $_ -match '曲线=cpu' })[1]
            if ((Num $spikeLine '进程PID') -eq $r.Burner.Id) { break }
            if ($attempt -lt 2) { "  这次没有在曲线上找到忙循环进程的点，重试一次" }
        }
        $hook | ForEach-Object { "  $_" }
        $cpu = @($hook | Where-Object { $_ -match '曲线=cpu' })
        $peak = $cpu[0]; $spike = $cpu[1]; $newest = $cpu[2]; $left = $cpu[3]
        Check ($peak -match '状态=有数据' -and (Word $peak '窗口') -eq $key) "peak：有数据，窗口 $key"
        Check ((Num $peak '数值') -gt 0 -and (Num $peak '数值') -le 100) "整条曲线最高处数值 $(Num $peak '数值') 在 0～100 之间"
        Check-Height $peak 'peak'
        # 忙循环（所有逻辑处理器自旋、高优先级）那几秒里它就是占用最高的进程：悬停在记着它的点上，提示的进程 PID 是它，数值是一次尖峰。
        # 本机可能同时有别的满载进程，所以不要求它是整条曲线的最高点，只要求曲线上记着它的点（且数值是尖峰）。
        Check ($spike -match '状态=有数据' -and (Num $spike '进程PID') -eq $r.Burner.Id) "尖峰处提示的进程 PID $(Word $spike '进程PID') 就是忙循环进程 $($r.Burner.Id)"
        Check ((Num $spike '数值') -ge 50) "该点数值 $(Num $spike '数值')% 是一次 CPU 尖峰"
        Check-Height $spike '忙循环尖峰处'
        Check ($newest -match '状态=有数据' -and (Num $newest '点距今') -lt 3.5) "最右侧的点距现在 $(Num $newest '点距今') 秒（不超过一个采样间隔）"
        Check-Height $newest 'cpu 最右'
        Check ($left -match '状态=无数据') '曲线最左（还没有数据的位置）是“无数据”'
        $mem = $hook | Where-Object { $_ -match '曲线=memory' } | Select-Object -First 1
        Check ($mem -match '状态=有数据' -and (Word $mem '进程PID') -eq '无') '内存曲线有数据，没有进程'
        Check-Height $mem 'memory'
        $net = $hook | Where-Object { $_ -match '曲线=network' } | Select-Object -First 1
        Check ($net -match '状态=有数据' -and (Word $net '进程PID') -eq '无') '网络曲线有数据，没有进程（项目没有进程级网络数据）'
        $disk = $hook | Where-Object { $_ -match '曲线=disk' } | Select-Object -First 1
        Check ($disk -match '状态=有数据') '磁盘曲线有数据'
        $gpu = $hook | Where-Object { $_ -match '曲线=gpu|没有这类曲线' } | Select-Object -Last 1
        Check ($null -ne $gpu) 'GPU 曲线有读数（或本机没有 GPU）'
        Check (-not ($r.Lines -join "`n" -match '最高：|PID \d+）')) '日志里没有提示卡片的文字（进程名只在界面显示）'
    }

    "== 2. 真实鼠标事件路径：PostMessage WM_MOUSEMOVE / WM_MOUSELEAVE =="
    $app = Start-App 'mouse' '{"history_window": "60s", "page": 1}'
    Start-Sleep -Seconds 5
    $rc = New-Object HoverWin+RECT
    [void][HoverWin]::GetClientRect($app.Handle, [ref]$rc)
    # 曲线区域在窗口右侧偏上：横向约 96%（最新数据一侧，只运行了几秒，曲线只铺在最右侧一小段），纵向约 40%。
    $x = [int]($rc.Right * 0.96); $y = [int]($rc.Bottom * 0.40)
    foreach ($dx in 0, -10, -20) { [void][HoverWin]::PostMessage($app.Handle, 0x0200, [IntPtr]::Zero, [IntPtr](($y -shl 16) -bor (($x + $dx) -band 0xFFFF))); Start-Sleep -Milliseconds 150 }
    Start-Sleep -Milliseconds 800
    $lines = Log-Lines $app
    $enter = $lines | Where-Object { $_ -match '悬停 进入' } | Select-Object -First 1
    "  日志：$enter"
    Check ($null -ne $enter -and $enter -match '曲线=cpu') '鼠标移到曲线上，日志里有“悬停 进入 曲线=cpu”'
    if ($null -ne $enter) { Check ($enter -match '状态=有数据') '最新一侧有数据' }
    [void][HoverWin]::PostMessage($app.Handle, 0x02A3, [IntPtr]::Zero, [IntPtr]::Zero)   # WM_MOUSELEAVE
    Start-Sleep -Milliseconds 800
    Check ($null -ne (Log-Lines $app | Where-Object { $_ -match '悬停 离开' })) '鼠标离开后日志里有“悬停 离开”'
    Close-App $app
}
finally {
    foreach ($p in $procs) { if ($p -and -not $p.HasExited) { $p.Kill() } }
    Start-Sleep -Milliseconds 300
    Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
}

if ($failures.Count -gt 0) { "失败 $($failures.Count) 项"; exit 1 }
'全部通过'
