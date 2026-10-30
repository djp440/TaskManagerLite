# 导出 CSV 与复制所选行的端到端验收（#82）。
# 做法：启动一个带逗号、引号、空格的命令行的子进程（自己的 powershell.exe，隐藏窗口），
# 用 `--export-csv <process|performance>=<路径>` 启动应用实例（隔离的 TML_SETTINGS_DIR / TML_LOG_DIR，窗口在屏幕外，
# 不弹保存对话框和敏感列提示，等采样就绪后直接写文件并退出），再用 PowerShell 读文件核对：
#   1. 平铺视图：UTF-8 BOM、CRLF、表头与界面列一致（含已选的详细信息列）、按名称升序、PID 唯一、
#      子进程的命令行（含逗号、引号）经 Import-Csv 还原后与系统记录的完全一致。
#   2. 进程树视图：多出“层级”列，层级满足先序遍历（每行的层级不超过上一行加 1），子进程的层级 >= 1。
#   3. 分组视图：多出“分组”“层级”列，应用在前、后台进程在后，后台进程层级为 0，应用内部层级满足先序遍历。
#   4. 性能快照：表头、各资源的当前数值、曲线数据（时间窗口内、按时间升序、距今在 [-60, 0]）、内存使用率与已用/总量一致。
#   5. 写文件失败（目录不存在）：不崩溃，日志记录错误码 3；不写任何内容。
#   6. 复制所选行（`--menu-action copy-row:<pid>`，平铺与分组）：剪贴板里是制表符分隔的一行，列与导出一致。
#   7. 键盘 Ctrl+C（真实按键，可用 -SkipKeyboard 跳过）：进程页用方向键选中一行后 Ctrl+C 复制该行；搜索框有焦点时不触发复制所选行。
#   8. 界面操作（真实鼠标与按键，同样受 -SkipKeyboard 控制）：右键表头 -> “导出为 CSV…” -> 敏感列提示（Esc 取消不弹保存对话框；
#      “继续导出”后弹出系统保存对话框，Esc 取消不报错；粘贴路径回车后写出文件）；性能页点“导出当前快照…”保存。
#      按钮位置按客户区尺寸、居中的对话框推算（窗口宽度 >= 900 时侧边栏 160 逻辑像素）。
# 只结束自己启动的进程；剪贴板原有文本在结束时还原。
# 用法：pwsh scripts/test-export-csv.ps1 [-Exe target/release/TaskManagerLite.exe] [-SkipKeyboard]
# 键盘部分需要交互式桌面，且运行期间不要动键盘鼠标和窗口焦点。
param(
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe",
    [switch]$SkipKeyboard
)

$ErrorActionPreference = 'Stop'
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class Win {
    [StructLayout(LayoutKind.Sequential)] struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr extra; }
    [StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort vk, scan; public uint flags, time; public IntPtr extra; }
    [StructLayout(LayoutKind.Explicit)] struct InputUnion { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
    [StructLayout(LayoutKind.Sequential)] struct INPUT { public uint type; public InputUnion u; }
    [DllImport("user32.dll")] static extern uint SendInput(uint n, INPUT[] inputs, int size);
    [DllImport("user32.dll")] static extern uint MapVirtualKey(uint code, uint type);
    [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    public static void Chord(params ushort[] vks) {
        foreach (var vk in vks) Send(vk, false);
        System.Threading.Thread.Sleep(30);
        for (int i = vks.Length - 1; i >= 0; i--) Send(vks[i], true);
    }
    static void Send(ushort vk, bool up) {
        var inp = new INPUT[1];
        inp[0].type = 1;
        inp[0].u.ki.vk = vk;
        inp[0].u.ki.scan = (ushort)MapVirtualKey(vk, 0);
        inp[0].u.ki.flags = up ? 2u : 0u;
        SendInput(1, inp, Marshal.SizeOf(typeof(INPUT)));
        System.Threading.Thread.Sleep(15);
    }
    public static bool Foreground(IntPtr h) {
        keybd_event(0x12, 0, 0, UIntPtr.Zero);
        keybd_event(0x12, 0, 2, UIntPtr.Zero);
        ShowWindow(h, 9);
        return SetForegroundWindow(h);
    }
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] static extern void mouse_event(uint f, uint dx, uint dy, uint d, UIntPtr e);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr h, System.Text.StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, System.Text.StringBuilder s, int n);
    public static void Click(int x, int y, bool right) {
        SetCursorPos(x, y); System.Threading.Thread.Sleep(100);
        mouse_event(right ? 8u : 2u, 0, 0, 0, UIntPtr.Zero); mouse_event(right ? 16u : 4u, 0, 0, 0, UIntPtr.Zero);
    }
    public static string ForegroundClass() { var sb = new System.Text.StringBuilder(256); GetClassName(GetForegroundWindow(), sb, 256); return sb.ToString(); }
    public static string ForegroundTitle() { var sb = new System.Text.StringBuilder(256); GetWindowText(GetForegroundWindow(), sb, 256); return sb.ToString(); }
    public static uint ForegroundPid() { uint pid; GetWindowThreadProcessId(GetForegroundWindow(), out pid); return pid; }
}
'@

[void][Win]::SetProcessDPIAware()
$exePath = (Resolve-Path $Exe).Path
$root = Join-Path ([System.IO.Path]::GetTempPath()) "tml-export-test-$(Get-Random)"
New-Item -ItemType Directory -Force $root | Out-Null
$failures = New-Object System.Collections.Generic.List[string]
function Check($ok, $msg) {
    if ($ok) { "  通过：$msg" } else { "  失败：$msg"; $script:failures.Add($msg) }
}
function WaitUntil([scriptblock]$cond, $sec) {
    $end = (Get-Date).AddSeconds($sec)
    while ((Get-Date) -lt $end) { if (& $cond) { return $true }; Start-Sleep -Milliseconds 100 }
    [bool](& $cond)
}

# 写一份隔离的设置目录；返回 @{Settings; Logs}。
function New-Env($name, $settingsJson) {
    $dir = Join-Path $root $name
    $s = Join-Path $dir 'settings'; $l = Join-Path $dir 'logs'
    New-Item -ItemType Directory -Force $s, $l | Out-Null
    if ($settingsJson) { Set-Content -Path (Join-Path $s 'settings.json') -Value $settingsJson -Encoding utf8 }
    @{ Settings = $s; Logs = $l; Dir = $dir }
}
function Log-Text($e) {
    $f = Get-ChildItem $e.Logs -Filter *.log -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($f) { Get-Content $f.FullName -Raw -Encoding utf8 } else { '' }
}
# 以导出钩子运行应用，等它自己退出；返回日志文本。
function Run-Export($e, $spec) {
    $env:TML_SETTINGS_DIR = $e.Settings
    $env:TML_LOG_DIR = $e.Logs
    $p = Start-Process -FilePath $exePath -ArgumentList '--export-csv', "`"$spec`"" -PassThru
    try {
        if (-not $p.WaitForExit(60000)) { $p.Kill(); [void]$p.WaitForExit(3000); Write-Host '  应用 60 秒内没有退出'; $script:failures.Add('导出钩子应在 60 秒内退出') }
    } finally { if (-not $p.HasExited) { $p.Kill() } }
    Log-Text $e
}
function Read-Bytes($f) { [System.IO.File]::ReadAllBytes($f) }
function Has-Bom($f) { $b = Read-Bytes $f; $b.Length -ge 3 -and $b[0] -eq 0xEF -and $b[1] -eq 0xBB -and $b[2] -eq 0xBF }
# 除引号内的换行外，所有换行都必须是 CRLF，且文件以 CRLF 结尾。
function Crlf-Ok($f) {
    $t = [System.IO.File]::ReadAllText($f, [System.Text.UTF8Encoding]::new($true))
    if (-not $t.EndsWith("`r`n")) { return $false }
    $inQuote = $false
    for ($i = 0; $i -lt $t.Length; $i++) {
        $c = $t[$i]
        if ($c -eq '"') { $inQuote = -not $inQuote }
        elseif ($c -eq "`n" -and -not $inQuote -and ($i -eq 0 -or $t[$i - 1] -ne "`r")) { return $false }
    }
    $true
}
function Ordinal-Lower($s) { $s.ToLowerInvariant() }
# 先序遍历：每行的层级不超过上一行加 1（第一行必须是 0）。
function Preorder-Ok($depths) {
    $prev = -1
    foreach ($d in $depths) { if ($d -gt $prev + 1 -or $d -lt 0) { return $false }; $prev = $d }
    $true
}

# 启动带窗口的实例（--allow-multiple），等主窗口出现；结果放在 $script:app / $script:main。
function Start-UiApp {
    $script:app = Start-Process $exePath -ArgumentList '--allow-multiple' -PassThru
    $script:main = [IntPtr]::Zero
    for ($i = 0; $i -lt 100 -and $script:main -eq [IntPtr]::Zero; $i++) {
        Start-Sleep -Milliseconds 100; $script:app.Refresh()
        if ($script:app.HasExited) { throw "实例提前退出，退出码 $($script:app.ExitCode)" }
        if ($script:app.MainWindowHandle -ne [IntPtr]::Zero -and [Win]::IsWindowVisible($script:app.MainWindowHandle)) { $script:main = $script:app.MainWindowHandle }
    }
    if ($script:main -eq [IntPtr]::Zero) { throw '等不到主窗口' }
}

$savedClip = try { Get-Clipboard -Raw -ErrorAction Stop } catch { $null }
$child = $null
$app = $null
try {
    # 子进程：命令行含空格、逗号和双引号（# 之后是 PowerShell 注释，不影响运行）。
    $tag = "tmlcsv$(Get-Random)"
    $childArgs = '-NoProfile -WindowStyle Hidden -Command "Start-Sleep 600 # ' + $tag + ',a \"q\" b"'
    $child = Start-Process powershell.exe -ArgumentList $childArgs -WindowStyle Hidden -PassThru
    Start-Sleep -Milliseconds 800
    if ($child.HasExited) { throw '子进程没有启动' }
    $expectedCmd = (Get-CimInstance Win32_Process -Filter "ProcessId=$($child.Id)").CommandLine
    "子进程 PID=$($child.Id) 命令行=$expectedCmd"
    $myPid = $child.Id

    $details = '"detail_columns":[{"id":"user"},{"id":"path"},{"id":"command_line"},{"id":"threads"},{"id":"cpu_time"}]'
    $detailTitles = @('用户名', '映像路径', '命令行', '线程', 'CPU 时间')
    $basic = @('名称', 'PID', 'CPU', '内存')   # 磁盘列标题带不带“≈”取决于是否有 ETW 数据，单独核对

    '=== 1. 平铺视图 ==='
    $e = New-Env 'flat' ('{"view":"flat","sort":{"key":"name","descending":false},"interval_index":0,' + $details + '}')
    $f = Join-Path $e.Dir '平铺 视图,含逗号.csv'
    $log = Run-Export $e "process=$f"
    Check (Test-Path $f) '生成了文件（文件名含中文、空格、逗号）'
    if (Test-Path $f) {
        Check (Has-Bom $f) '文件以 UTF-8 BOM 开头'
        Check (Crlf-Ok $f) '行尾为 CRLF'
        $rows = @(Import-Csv -Path $f -Encoding UTF8)
        $cols = @($rows[0].PSObject.Properties.Name)
        Check ($cols.Count -eq 10) "共 10 列：$($cols -join ' | ')"
        Check (($cols[0..3] -join ',') -eq ($basic -join ',')) '前四列标题与界面一致（名称、PID、CPU、内存）'
        Check ($cols[4] -like '磁盘*') "第五列是磁盘列（$($cols[4])）"
        Check (($cols[5..9] -join ',') -eq ($detailTitles -join ',')) '详细信息列的标题、顺序与所选一致'
        Check ($rows.Count -gt 20) "进程行数合理（$($rows.Count)）"
        $pids = $rows | ForEach-Object { $_.PID }
        Check (@($pids | Sort-Object -Unique).Count -eq $rows.Count) 'PID 唯一'
        $names = @($rows | ForEach-Object { Ordinal-Lower $_.'名称' })
        $sorted = $true
        for ($i = 1; $i -lt $names.Count; $i++) { if ([string]::CompareOrdinal($names[$i - 1], $names[$i]) -gt 0) { $sorted = $false; "    乱序：$($names[$i - 1]) > $($names[$i])"; break } }
        Check $sorted '按名称升序（与设置里的排序一致）'
        $mine = $rows | Where-Object { $_.PID -eq "$myPid" }
        Check ($null -ne $mine) '找到了测试子进程那一行'
        Check ($mine.'名称' -eq 'powershell.exe') "名称列是 powershell.exe（$($mine.'名称')）"
        Check ($mine.'命令行' -ceq $expectedCmd) '含逗号、引号的命令行与系统记录完全一致（Import-Csv 还原）'
        Check ($mine.'映像路径' -like '*powershell.exe') "映像路径列有值（$($mine.'映像路径')）"
        Check ($mine.'CPU' -match '^\d+(\.\d)?%$') "CPU 列是界面格式（$($mine.'CPU')）"
        Check ($mine.'内存' -match '^\d+\.\d MB$') "内存列是界面格式（$($mine.'内存')）"
        Check ($mine.'线程' -match '^\d+$') '线程列是数字'
        Check ($mine.'CPU 时间' -match '^\d+:\d\d:\d\d$') "CPU 时间列是界面格式（$($mine.'CPU 时间')）"
    }
    Check ($log -match '导出 进程列表 成功 行=\d+ 列=10') '日志只记录行数、列数与结果'
    Check ($log -notmatch [regex]::Escape($tag)) '日志里没有导出的内容（命令行标记）'

    '=== 2. 进程树视图 ==='
    $e = New-Env 'tree' ('{"view":"tree","sort":{"key":"pid","descending":false},"interval_index":0,"detail_columns":[{"id":"command_line"}]}')
    $f = Join-Path $e.Dir 'tree.csv'
    [void](Run-Export $e "process=$f")
    Check (Test-Path $f) '生成了文件'
    if (Test-Path $f) {
        $rows = @(Import-Csv -Path $f -Encoding UTF8)
        $cols = @($rows[0].PSObject.Properties.Name)
        Check (($cols[0..2] -join ',') -eq '层级,名称,PID') "表头以 层级,名称,PID 开头：$($cols -join ' | ')"
        $depths = @($rows | ForEach-Object { [int]$_.'层级' })
        Check (Preorder-Ok $depths) '层级满足先序遍历（缩进结构完整）'
        Check (@($depths | Where-Object { $_ -ge 1 }).Count -gt 0) '有子进程（层级 >= 1）'
        $mine = $rows | Where-Object { $_.PID -eq "$myPid" }
        Check ($null -ne $mine -and [int]$mine.'层级' -ge 1) "测试子进程是它父进程的后代（层级 $($mine.'层级')）"
        Check ($mine.'命令行' -ceq $expectedCmd) '树形视图里命令行同样还原一致'
    }

    '=== 3. 分组视图 ==='
    $e = New-Env 'group' ('{"view":"group","sort":{"key":"name","descending":false},"interval_index":0,"detail_columns":[{"id":"threads"}]}')
    $f = Join-Path $e.Dir 'group.csv'
    [void](Run-Export $e "process=$f")
    Check (Test-Path $f) '生成了文件'
    if (Test-Path $f) {
        $rows = @(Import-Csv -Path $f -Encoding UTF8)
        $cols = @($rows[0].PSObject.Properties.Name)
        Check (($cols[0..3] -join ',') -eq '分组,层级,名称,PID') "表头以 分组,层级,名称,PID 开头：$($cols -join ' | ')"
        $groups = @($rows | ForEach-Object { $_.'分组' })
        Check (@($groups | Where-Object { $_ -ne '应用' -and $_ -ne '后台进程' }).Count -eq 0) '分组列只有“应用”和“后台进程”'
        $firstBg = [array]::IndexOf($groups, '后台进程')
        $lastApp = [array]::LastIndexOf($groups, '应用')
        Check ($firstBg -ge 0) "有后台进程（$(@($groups | Where-Object { $_ -eq '后台进程' }).Count) 行）"
        Check ($lastApp -lt $firstBg) '应用在前、后台进程在后'
        $bg = @($rows | Where-Object { $_.'分组' -eq '后台进程' })
        Check (@($bg | Where-Object { $_.'层级' -ne '0' }).Count -eq 0) '后台进程平铺（层级均为 0）'
        $apps = @($rows | Where-Object { $_.'分组' -eq '应用' })
        if ($apps.Count -gt 0) {
            Check (Preorder-Ok @($apps | ForEach-Object { [int]$_.'层级' })) "应用内部层级满足先序遍历（应用 $($apps.Count) 行，其中后代 $(@($apps | Where-Object { $_.'层级' -ne '0' }).Count) 行，均已展开导出）"
        } else {
            '  （本机当前没有带窗口的应用，跳过应用层级核对）'
        }
        $mine = $rows | Where-Object { $_.PID -eq "$myPid" }
        Check ($null -ne $mine) "测试子进程出现在分组导出里（分组=$($mine.'分组') 层级=$($mine.'层级')）"
    }

    '=== 4. 性能快照 ==='
    $e = New-Env 'perf' '{"interval_index":0}'
    $f = Join-Path $e.Dir 'perf.csv'
    $log = Run-Export $e "performance=$f"
    Check (Test-Path $f) '生成了文件'
    if (Test-Path $f) {
        Check (Has-Bom $f) '文件以 UTF-8 BOM 开头'
        Check (Crlf-Ok $f) '行尾为 CRLF'
        $rows = @(Import-Csv -Path $f -Encoding UTF8)
        $cols = @($rows[0].PSObject.Properties.Name)
        Check (($cols -join ',') -eq '类型,资源,实例,指标,时间,距今（秒）,数值,单位') "表头：$($cols -join ',')"
        $cur = @($rows | Where-Object { $_.'类型' -eq '当前' })
        $ser = @($rows | Where-Object { $_.'类型' -eq '曲线' })
        $ids = @($cur | ForEach-Object { $_.'实例' } | Sort-Object -Unique)
        Check ($ids -contains 'cpu' -and $ids -contains 'memory') "当前数值覆盖 CPU 与内存（实例：$($ids -join ', ')）"
        Check ($ids | Where-Object { $_ -like 'disk*' }) '有磁盘当前数值'
        Check ($ids | Where-Object { $_ -like 'network*' }) '有网络当前数值'
        $gpu = @($cur | Where-Object { $_.'实例' -like 'gpu*' })
        if ($gpu.Count -gt 0) { Check ($gpu.Count -ge 3) "GPU 当前数值（$($gpu.Count) 行）" } else { '  （本机没有 GPU 实例，跳过）' }
        $cpuNow = @($cur | Where-Object { $_.'实例' -eq 'cpu' -and $_.'指标' -eq '利用率' })
        Check ($cpuNow.Count -eq 1 -and [double]$cpuNow[0].'数值' -ge 0 -and [double]$cpuNow[0].'数值' -le 100 -and $cpuNow[0].'单位' -eq '%') "CPU 当前利用率在 0–100（$($cpuNow[0].'数值')%）"
        $used = [double](@($cur | Where-Object { $_.'实例' -eq 'memory' -and $_.'指标' -eq '已用' })[0].'数值')
        $total = [double](@($cur | Where-Object { $_.'实例' -eq 'memory' -and $_.'指标' -eq '总量' })[0].'数值')
        $pct = [double](@($cur | Where-Object { $_.'实例' -eq 'memory' -and $_.'指标' -eq '使用率' })[0].'数值')
        Check ($total -gt 1GB -and $used -gt 0 -and $used -le $total) "内存已用/总量合理（$([math]::Round($used / 1GB, 1)) / $([math]::Round($total / 1GB, 1)) GB）"
        Check ([math]::Abs($used / $total * 100 - $pct) -lt 0.01) '内存使用率与已用/总量一致'
        Check ($cur | Where-Object { $_.'时间' -match '^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d$' -and $_.'距今（秒）' -eq '0' }) '当前行的时间为本地时间，距今 0'
        $cpuSer = @($ser | Where-Object { $_.'实例' -eq 'cpu' })
        Check ($cpuSer.Count -ge 3) "CPU 曲线有多个点（$($cpuSer.Count)，采样间隔 0.5 秒、运行约 5 秒）"
        $offs = @($cpuSer | ForEach-Object { [double]$_.'距今（秒）' })
        Check (($offs | Where-Object { $_ -lt -60 -or $_ -gt 0 }).Count -eq 0) '曲线点都在 60 秒窗口内（距今 [-60, 0]）'
        $asc = $true; for ($i = 1; $i -lt $offs.Count; $i++) { if ($offs[$i] -lt $offs[$i - 1]) { $asc = $false } }
        Check $asc '曲线数据按时间升序'
        Check (@($ser | Where-Object { $_.'时间' -notmatch '^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d$' }).Count -eq 0) '曲线每个点都有本地时间'
        Check (@($ser | Where-Object { $_.'数值' -notmatch '^-?\d+(\.\d+)?$' }).Count -eq 0) '曲线数值都是数字'
        Check (@($ser | Where-Object { $_.'实例' -eq 'memory' }).Count -ge 3) '内存曲线有多个点'
    }
    Check ($log -match '导出 性能快照 成功 行=\d+ 列=8') '日志记录行数、列数与结果'

    '=== 5. 写文件失败 ==='
    $e = New-Env 'fail' ''
    $f = Join-Path $e.Dir '不存在的目录\a.csv'
    $log = Run-Export $e "process=$f"
    Check (-not (Test-Path $f)) '没有写出文件'
    Check ($log -match '导出 进程列表 失败 错误码=3') '日志记录失败与错误码 3（找不到路径），应用没有崩溃'

    '=== 6. 复制所选行（--menu-action copy-row） ==='
    $copyEnv = New-Env 'copy' ''
    $env:TML_SETTINGS_DIR = $copyEnv.Settings; $env:TML_LOG_DIR = $copyEnv.Logs
    function Copy-Row($suffix) {
        Set-Clipboard -Value 'TML-SENTINEL'
        $p = Start-Process $exePath -ArgumentList '--menu-action', "copy-row:${myPid}${suffix}" -PassThru
        $end = (Get-Date).AddSeconds(25); $got = $null
        while ((Get-Date) -lt $end -and -not $got) {
            Start-Sleep -Milliseconds 300
            $c = Get-Clipboard -Raw
            if ($c -and $c -ne 'TML-SENTINEL') { $got = $c }
        }
        if (-not $p.HasExited) { $p.Kill(); [void]$p.WaitForExit(3000) }
        $got
    }
    $got = Copy-Row ''
    if (-not $got) { '  （剪贴板没有拿到内容，重试一次）'; $got = Copy-Row '' }
    $cells = if ($got) { $got.TrimEnd("`r", "`n") -split "`t" } else { @() }
    Check ($cells.Count -eq 5) "平铺视图：制表符分隔的 5 列（名称、PID、CPU、内存、磁盘）：$got"
    if ($cells.Count -eq 5) {
        Check ($cells[0] -eq 'powershell.exe' -and $cells[1] -eq "$myPid") '名称与 PID 对应所选进程'
        Check ($cells[2] -match '%$' -and $cells[3] -match ' MB$' -and $cells[4] -match ' MB/s$') 'CPU、内存、磁盘使用界面格式'
    }
    Check ($got -and $got.TrimEnd("`r", "`n") -notmatch "`n") '只有一行，没有表头'
    $got = Copy-Row ':group'
    if (-not $got) { '  （剪贴板没有拿到内容，重试一次）'; $got = Copy-Row ':group' }
    $cells = if ($got) { $got.TrimEnd("`r", "`n") -split "`t" } else { @() }
    Check ($cells.Count -eq 7) "分组视图：7 列（分组、层级 + 基本五列）：$got"
    if ($cells.Count -eq 7) {
        Check (@('应用', '后台进程') -contains $cells[0] -and $cells[1] -match '^\d+$') "分组与层级列（$($cells[0]) / $($cells[1])）"
        Check ($cells[2] -eq 'powershell.exe' -and $cells[3] -eq "$myPid") '分组视图里复制的仍是所选进程'
    }

    if (-not $SkipKeyboard) {
        '=== 7. 键盘 Ctrl+C（真实按键） ==='
        $kEnv = New-Env 'keys' '{"view":"flat","sort":{"key":"pid","descending":false},"interval_index":1}'
        $env:TML_SETTINGS_DIR = $kEnv.Settings; $env:TML_LOG_DIR = $kEnv.Logs
        $env:TML_INSTANCE_ID = "exportcsv$(Get-Random)"
        Start-UiApp
        function Focus-App {
            for ($i = 0; $i -lt 10; $i++) {
                [void][Win]::Foreground($script:main); Start-Sleep -Milliseconds 150
                if ([Win]::ForegroundPid() -eq [uint32]$script:app.Id) { return }
            }
            throw '无法把应用窗口带到前台（需要交互式桌面，且运行期间不要切换窗口）'
        }
        function Key([uint16[]]$vks) { Focus-App; [Win]::Chord($vks) }
        $kLog = { $f = Get-ChildItem $kEnv.Logs -Filter *.log | Select-Object -First 1; Get-Content $f.FullName -Raw -Encoding utf8 }
        Check (WaitUntil { (& $kLog) -match '首个快照帧' } 10) '应用已出首个快照'
        # 平铺按 PID 升序，向下一格选中第一行（System Idle Process，PID 0）。
        Key @([uint16]0x28)
        Start-Sleep -Milliseconds 300
        Set-Clipboard -Value 'TML-SENTINEL'
        Key @([uint16]0x11, [uint16]0x43)   # Ctrl+C
        $ok = WaitUntil { (Get-Clipboard -Raw) -ne 'TML-SENTINEL' } 5
        $clip = Get-Clipboard -Raw
        $cells = if ($clip) { $clip.TrimEnd("`r", "`n") -split "`t" } else { @() }
        Check ($ok -and $cells.Count -eq 5 -and $cells[1] -match '^\d+$') "进程页 Ctrl+C 复制了所选行（5 列）：$clip"
        Check ((& $kLog) -match '复制所选行 成功 列=5') '日志记录复制成功'
        # 搜索框有焦点时 Ctrl+C 不触发“复制所选行”。
        $before = ([regex]::Matches((& $kLog), '复制所选行 成功')).Count
        Set-Clipboard -Value 'TML-SENTINEL'
        Key @([uint16]0x11, [uint16]0x46)   # Ctrl+F：聚焦搜索框
        Start-Sleep -Milliseconds 300
        Key @([uint16]0x11, [uint16]0x43)
        Start-Sleep -Milliseconds 800
        $after = ([regex]::Matches((& $kLog), '复制所选行 成功')).Count
        Check ($after -eq $before) '搜索框有焦点时 Ctrl+C 不复制所选行'
        Check ((Get-Clipboard -Raw).Trim() -eq 'TML-SENTINEL') '剪贴板保持不变（搜索框里没有选中文字）'
        Check (-not $app.HasExited) '整个过程应用没有崩溃'

        '=== 8. 界面操作：菜单、敏感列提示、系统保存对话框 ==='
        $app.Kill(); [void]$app.WaitForExit(3000)
        $uEnv = New-Env 'ui' '{"view":"flat","sort":{"key":"pid","descending":false},"interval_index":1,"detail_columns":[{"id":"command_line"}]}'
        $env:TML_SETTINGS_DIR = $uEnv.Settings; $env:TML_LOG_DIR = $uEnv.Logs
        Start-UiApp
        $uLog = { $f = Get-ChildItem $uEnv.Logs -Filter *.log | Select-Object -First 1; Get-Content $f.FullName -Raw -Encoding utf8 }
        $count = { param($pat) ([regex]::Matches((& $uLog), $pat)).Count }
        Check (WaitUntil { (& $uLog) -match '首个快照帧' } 10) '应用已出首个快照'
        $o = New-Object Win+POINT; [void][Win]::ClientToScreen($main, [ref]$o)
        $cr = New-Object Win+RECT; [void][Win]::GetClientRect($main, [ref]$cr)
        $sc = [Win]::GetDpiForWindow($main) / 96.0
        $lw = $cr.R / $sc; $lh = $cr.B / $sc    # 客户区的逻辑尺寸
        function Click-At($lx, $ly, $right) { Focus-App; [Win]::Click([int]($o.X + $lx * $sc), [int]($o.Y + $ly * $sc), $right) }
        $sidebar = if ($lw -ge 900) { 160 } else { 48 }
        # 表头右键 -> 菜单里按 Up 选中最后一项“导出为 CSV…” -> 回车。
        function Open-ExportFromHeader {
            Click-At ($sidebar + 60) 73 $true
            Start-Sleep -Milliseconds 600
            [Win]::Chord([uint16]0x26); Start-Sleep -Milliseconds 250   # 不能再 Focus-App：重新前置会关掉弹出的菜单
            [Win]::Chord([uint16]0x0D)
        }
        # 敏感列提示里的“继续导出”：对话框在窗口中央，按钮在中心偏右下（147, 52）逻辑像素。
        function Click-Continue { Click-At ($lw / 2 + 147) ($lh / 2 + 52) $false }
        function Wait-SaveDialog {
            $ok = WaitUntil { [Win]::ForegroundClass() -eq '#32770' -and [Win]::ForegroundTitle() -like '*CSV*' } 8
            Start-Sleep -Milliseconds 700   # 对话框刚出现时还在初始化，太早的按键会被丢掉
            $ok
        }

        $b = & $count '含敏感列'
        Open-ExportFromHeader
        Check (WaitUntil { (& $count '含敏感列') -gt $b } 5) '表头右键菜单的“导出为 CSV…”弹出敏感列提示（命令行）'
        Key @([uint16]0x1B)   # Esc
        Start-Sleep -Milliseconds 800
        Check ((& $count '打开保存对话框') -eq 0) 'Esc 取消提示后没有弹出保存对话框'

        Open-ExportFromHeader
        Check (WaitUntil { (& $count '含敏感列') -ge 2 } 5) '再次打开敏感列提示'
        Click-Continue
        Check (Wait-SaveDialog) '点“继续导出”后弹出系统保存对话框（标题含 CSV）'
        Check ((& $count '敏感列已确认') -eq 1) '日志记录敏感列已确认'
        [Win]::Chord([uint16]0x1B)   # Esc 取消保存
        Check (WaitUntil { (& $count '已取消') -ge 1 } 5) '取消保存：日志记为已取消'
        Check ($app.HasExited -eq $false -and (& $count '失败') -eq 0) '取消保存不报错、不崩溃'

        $saved = Join-Path $uEnv.Dir 'ui 导出,进程.csv'
        Open-ExportFromHeader
        Check (WaitUntil { (& $count '含敏感列') -ge 3 } 5) '第三次打开敏感列提示'
        Click-Continue
        Check (Wait-SaveDialog) '再次弹出保存对话框'
        Set-Clipboard -Value $saved
        [Win]::Chord([uint16]0x11, [uint16]0x56); Start-Sleep -Milliseconds 400   # Ctrl+V 粘贴完整路径
        [Win]::Chord([uint16]0x0D)
        Check (WaitUntil { Test-Path $saved } 8) '回车后写出了文件（路径含中文、空格、逗号）'
        Check (WaitUntil { (& $count '成功 行=\d+ 列=6') -ge 1 } 5) '日志记录成功、行数与 6 列（名称、PID、CPU、内存、磁盘、命令行）'
        if (Test-Path $saved) {
            Check (Has-Bom $saved) '文件有 UTF-8 BOM'
            $h = @((Import-Csv -Path $saved -Encoding UTF8)[0].PSObject.Properties.Name)
            Check ($h[5] -eq '命令行' -and $h[0] -eq '名称') "表头：$($h -join ' | ')"
        }

        # 性能页：Ctrl+2，左栏底部的“导出当前快照…”。
        Key @([uint16]0x11, [uint16]0x32); Start-Sleep -Milliseconds 800
        Click-At (($sidebar + 12) + 120) ($lh - 28 - 12 - 16) $false
        Check (Wait-SaveDialog) '性能页“导出当前快照…”弹出保存对话框（没有敏感列提示）'
        $savedPerf = Join-Path $uEnv.Dir 'ui 性能快照.csv'
        Set-Clipboard -Value $savedPerf
        [Win]::Chord([uint16]0x11, [uint16]0x56); Start-Sleep -Milliseconds 400
        [Win]::Chord([uint16]0x0D)
        Check (WaitUntil { Test-Path $savedPerf } 8) '写出了性能快照文件'
        if (Test-Path $savedPerf) {
            $rows = @(Import-Csv -Path $savedPerf -Encoding UTF8)
            Check ((@($rows[0].PSObject.Properties.Name) -join ',') -eq '类型,资源,实例,指标,时间,距今（秒）,数值,单位' -and $rows.Count -gt 10) "性能快照表头与行数（$($rows.Count)）"
        }
        Check (-not $app.HasExited) '整个界面操作过程应用没有崩溃'
    }
}
finally {
    if ($app -and -not $app.HasExited) { $app.Kill(); [void]$app.WaitForExit(3000) }
    if ($child -and -not $child.HasExited) { $child.Kill(); [void]$child.WaitForExit(3000) }
    if ($null -ne $savedClip) { Set-Clipboard -Value $savedClip } else { Set-Clipboard -Value $null }
    Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
    Remove-Item Env:TML_INSTANCE_ID -ErrorAction SilentlyContinue
}

if ($failures.Count -gt 0) { "失败 $($failures.Count) 项："; $failures | ForEach-Object { "  - $_" }; exit 1 }
'全部通过'
