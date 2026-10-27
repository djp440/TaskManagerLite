# 本机实测：把采集代码（examples/dump_memory）的输出与系统自带的数据源对比，任一项超出容差则以非零退出。
# - 硬件：Win32_PhysicalMemory / Win32_PhysicalMemoryArray（WMI 只在这里用，程序本身不使用 WMI）。
# - 动态值：Win32_PerfRawData_PerfOS_Memory（即 \Memory\* 性能计数器，英文属性名，不受系统语言影响）、
#   Memory Compression 进程的工作集。
# 用法：pwsh scripts/verify-memory.ps1
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

$ours = @{}
cargo run -q -p tml-core --example dump_memory | ForEach-Object {
    $k, $v = $_ -split '=', 2
    $ours[$k] = $v
}
$fail = 0
function Check([string]$name, $actual, $expected, [double]$tolerance) {
    $diff = [math]::Abs([double]$actual - [double]$expected)
    $ok = $diff -le $tolerance
    if (-not $ok) { $script:fail++ }
    '{0,-6} {1,-14} 本程序 {2,16:N0}  系统 {3,16:N0}  差 {4,14:N0}  容差 {5:N0}' -f $(if ($ok) { 'OK' } else { 'FAIL' }), $name, [double]$actual, [double]$expected, $diff, $tolerance
}

# 硬件信息：必须完全一致。
$modules = @(Get-CimInstance Win32_PhysicalMemory)
$array = Get-CimInstance Win32_PhysicalMemoryArray | Select-Object -First 1
Check '插槽总数' $ours.slots_total $array.MemoryDevices 0
Check '已用插槽' $ours.slots_used $modules.Count 0
$capacity = ($modules | Measure-Object Capacity -Sum).Sum
Check '安装容量' $ours.installed $capacity 0
$configured = ($modules | ForEach-Object { if ($_.ConfiguredClockSpeed) { $_.ConfiguredClockSpeed } else { $_.Speed } } | Measure-Object -Minimum).Minimum
Check '速度MT/s' $ours.speed_mts $configured 0
$formNames = @{ 8 = 'DIMM'; 12 = 'SODIMM'; 11 = 'RIMM'; 13 = 'SRIMM'; 14 = 'FB-DIMM'; 7 = 'SIMM'; 5 = 'Row of chips'; 6 = 'Row of chips' }
$formWmi = $formNames[[int]$modules[0].FormFactor]
Check '外形规格' ([int]($ours.form_factor -eq $formWmi)) 1 0
"外形规格 本程序={0} WMI={1}（FormFactor={2}）  内存类型 本程序={3} WMI SMBIOSMemoryType={4}" -f $ours.form_factor, $formWmi, $modules[0].FormFactor, $ours.memory_type, $modules[0].SMBIOSMemoryType

# 动态值：两次读取之间会变化，按总量的比例给容差。
$perf = Get-CimInstance Win32_PerfRawData_PerfOS_Memory
$total = [double]$ours.total
$tol = [math]::Max(0.02 * $total, 256MB)
# 变化缓慢的项（提交限制、内核池、已压缩）用更紧的容差。
$tight = 128MB
Check '可用' $ours.available $perf.AvailableBytes $tol
Check '已提交' $ours.committed $perf.CommittedBytes $tol
Check '提交限制' $ours.commit_limit $perf.CommitLimit $tight
Check '分页池' $ours.paged_pool $perf.PoolPagedBytes $tight
Check '非分页池' $ours.nonpaged_pool $perf.PoolNonpagedBytes $tight
$standby = $perf.StandbyCacheCoreBytes + $perf.StandbyCacheNormalPriorityBytes + $perf.StandbyCacheReserveBytes
Check '备用' $ours.standby $standby $tol
Check '已修改' $ours.modified $perf.ModifiedPageListBytes $tol
Check '空闲' $ours.free $perf.FreeAndZeroPageListBytes $tol
Check '已缓存' ([double]$ours.standby + [double]$ours.modified) ($standby + $perf.ModifiedPageListBytes) $tol
$mc = Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter "Name='Memory Compression'"
if ($mc) { Check '已压缩' $ours.compressed $mc.WorkingSet $tight } else { '跳过已压缩：没有 Memory Compression 进程' }
if ($fail -gt 0) { "$fail 项不一致"; exit 1 }
'全部通过'
