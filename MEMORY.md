# 开发经验

## 2026-10-28 12:35 托盘常驻与后台采样
- 采样策略是纯函数 `sampling::plan(可见性, 后台开关, 暂停)`：`Full` / `MetricsOnly`（跳过 `SystemProcessInformation` 枚举与窗口枚举，约 10 ms/s 对前台约 27 ms/s）/ `Idle`。`Sampler` 的进程基线（时间、整机 CPU 总时间）与系统基线分开，夹着只采指标的采样时进程占用率仍按上一次进程快照做差。`Sample::processes_fresh` 为假时控制器不动进程快照与界面。
- 曲线连续性的可观测量：恢复可见后第一次完整采样写日志 `恢复可见 … 断点=N 最大间隔=… ms`，`scripts/test-background-sampling.ps1`、`scripts/test-tray.ps1` 都据此断言。
- 托盘直接用 `Shell_NotifyIconW`（`tray.rs`），不用 `tray-icon`：最小用法也使 release exe +106 KB 并引入第二份 muda 0.21；自己实现整个 #8（含窗口显示/隐藏逻辑）只 +14.8 KB。回调窗口用隐藏的顶层窗口而不是 message-only 窗口，后者收不到 `TaskbarCreated` 广播（资源管理器重启后图标不会恢复）。图标运行时按像素绘制（`CreateIcon`，非预乘 BGRA、AND 掩码全 0），无资源文件。
- 隐藏到托盘后“最后一个窗口关闭”不再该结束事件循环：`ui.run()` 换成 `ui.show()` + `slint::run_event_loop_until_quit()`，所有退出路径显式 `quit_event_loop()`（普通关闭、托盘“退出”、提权重启）。`Window::hide()` 在 Windows/winit 上只是 `set_visible(false)`，不销毁窗口；可见性不能只看 `is_minimized()`，要同时看自己维护的 `hidden`。
- 单实例唤起：`activate_own_window` 只找可见窗口，隐藏在托盘时要先 `ui.show()`，所以监听回调改走 `Controller::show_window`。`TML_INSTANCE_ID` 环境变量给互斥量/事件名加后缀，测试可以在已有真实实例的机器上验证单实例。
- 托盘自动化：向托盘窗口 `PostMessage(WM_APP+1, uID=1, WM_LBUTTONUP/WM_RBUTTONUP)` 等价于真实点击；右键会进入模态菜单，截图后用 `WM_CANCELMODE` 取消；菜单命令可直接 `PostMessage(WM_COMMAND, 1002)`。`Shell_NotifyIconGetRect` 返回 S_OK 或 S_FALSE（在溢出区）都说明图标已登记，NIM_ADD 是异步的，要轮询。PowerShell 双引号字符串里的弯引号 “ ” 会被当成引号，脚本里用「」。
- `command_line` 集成测试 `quoted_arguments_are_preserved` 偶发失败：ping 无效主机名会立刻退出，读命令行有竞态（与本改动无关）。

## 2026-10-28 12:30 其他虚拟桌面上的应用（#40）
- 其他桌面上的窗口和挂起的 UWP 窗口一样是 `DWMWA_CLOAKED=2`。区分靠 `IVirtualDesktopManager::IsWindowOnCurrentVirtualDesktop`：挂起的 UWP（计算器、Cortana、Text Input）都回答“在当前桌面”，无效句柄也回答 TRUE，所以“明确 FALSE”才算在其他桌面，不会误把挂起 UWP 算成应用。
- 第一次调用该接口就会把 `twinapi.dll` 等拉进进程：常驻工作集约 +4 MB（私有基本不变），而且任何 Win10 上几乎总有被 cloak 的挂起 UWP 窗口，会无条件触发。所以先读 `HKCU\...\Explorer\VirtualDesktops\VirtualDesktopIDs`（16 字节一个桌面）：不足 2 个桌面（含值不存在）就不碰 COM；读取出错才按多桌面处理。单桌面机器的分组视图内存与 main 无差别。
- COM 在采样线程第一次需要时用 MTA 初始化，`IVirtualDesktopManager` 存在 thread_local 里只创建一次；创建或查询失败后按 1s 起翻倍（最长 60s）的退避重试，只计连续失败、没有永久放弃的上限；清零只发生在查询成功时，创建成功不清零（否则某窗口的查询持续失败会形成“创建成功→查询失败→退避 1 秒→再创建”的循环，退避爬不上去）（开机自启时外壳未就绪、资源管理器重启都能恢复）；线程已用别的模型初始化（`RPC_E_CHANGED_MODE`）时沿用且不反初始化。
- `MoveWindowToDesktop` 通常只允许移动调用方进程自己的窗口：测试用自己创建的窗口，记事本只尝试一次，被拒绝就说明并跳过。

## 2026-10-28 12:20 ETW 磁盘速率（#5）
- 评审后的形态：设置项 `accurate_disk_io` 默认关闭，关闭时完全不碰 ETW（不清残留、不装钩子）；`Sampler::set_accurate_disk` 支持运行中开关。聚合器所有表有容量上限（窗口不可见时不取数也不会无限增长）；每窗口无法归属的事件超过 20% 就回报近似。崩溃路径（panic 钩子、异常过滤器）不分配内存：启动成功后预先泄漏停止用的 properties 缓冲和 UTF-16 名称。残留会话判定保守：只有 PID 不存在（`ERROR_INVALID_PARAMETER`）、创建时间对不上或确认已退出才清，拒绝访问视为存活。会话名带序号，避免旧会话收尾线程误停同名新会话。
- 私有内核会话：`LogFileMode = REAL_TIME | SYSTEM_LOGGER_MODE`、`EnableFlags = DISK_IO | THREAD`，会话名带 PID 与创建时间，不占 “NT Kernel Logger”。磁盘事件头里的进程号不可靠，用版本 3 负载的 `IssuingThreadId` 查线程表；缓存写入由 System 的惰性写入器发出，归属 System（与资源监视器一致）。详见 ADR-0004。
- ETW 会话属于系统：进程被 `TerminateProcess` 后会话残留。兜底是 `TraceGuard`（正常退出）、panic 钩子、未处理异常过滤器，以及下次启动按名称里的 PID + 创建时间扫残留会话（仅在开关开启且提权、启动会话时）。`measure-idle.ps1` 先 `CloseMainWindow` 再 `Kill`，避免提权测量时留下会话。
- 开发环境未提权时只能验证回退与单测：未提权调用 `StartTraceW` 返回 5（拒绝访问），说明属性结构已通过参数校验。事件偏移（版本 3 负载、64/32 位指针）和线程枚举事件来自文档，提权下的真实数据由 `scripts/test-etw-disk.ps1` 与人工验收核对。
- `windows` crate 的 `SetUnhandledExceptionFilter`/`EXCEPTION_POINTERS` 需要 `Win32_System_Kernel` 特性，`EVENT_TRACE_LOGFILEW` 需要 `Win32_System_Time`。
- `command_line` 集成测试 `quoted_arguments_are_preserved` 并行时偶发失败（单独重跑通过），与本改动无关。

## 2026-10-28 11:02 窄窗口工具栏
- 档位（提权按钮收成图标、隐藏品牌）由各部分的首选/最小宽度与 `root.width` 算出。被测量的元素如果参与窗口布局，会形成“档位 → 布局约束 → 窗口约束 → 窗口宽度 → 档位”的绑定循环。给工具栏布局和隐藏的测量 `Text` 显式设置 `x/y`，它们的约束就不再传给父元素。判断只用不随档位变化的量。
- `Text` 在 clip、不换行时最小宽度等于首选宽度，所以“布局 min-width ≤ 窗口宽度”就等价于没有控件被裁剪。截图模式和单元测试都断言 `toolbar-min-width`。
- `if` 条件元素只在渲染或处理事件时增删实例。不渲染的测试里改属性后，要派发一次 `PointerExited`，否则布局仍按旧实例计算。
- 单元测试用 `MinimalSoftwareWindow` + 自定义 `Platform`（`set_platform` 每个线程一次）逐像素扫宽度，文字宽度来自系统字体，与真实窗口一致（`toolbar_fit.rs`）。
- Slint 1.18 的 `Tooltip` 用文本模式会引入 markdown 解析，这里放自定义内容（一个 Rectangle）。提示固定出现在指针下方 8px、延迟 500ms，release 约增加 59 KB。
- `%TEMP%\tml-integ\lib.ps1` 点引入时会在同一脚本作用域里覆盖 `$script:Exe/$script:Out`。调用脚本的参数不要用这两个名字，否则会悄悄换成主仓库的旧 exe。

## 2026-10-28 08:57 第二批集成（CPU/内存详情、分组视图、多 PR 合并）
- Slint 的 `changed` 回调在组件创建时不触发：初始尺寸要在 `init` 里上报一次，否则曲线按默认 100×50 生成。
- 网格布局依赖模型时，组件创建那一刻模型还是空的，算出的是 1×1 的格子；按这个尺寸生成的曲线会被裁剪。Slint 只上报网格区的尺寸，格子尺寸在 Rust 里用布局同一个纯函数（`grid_plan`，经 pure callback 给 Slint）计算。截图模式在出图前断言曲线尺寸等于格子尺寸。
- 逐核网格选列数：在满足最小格宽 64px、格高 24px 的选项里，先选空格最少的，再选宽高比最接近 2:1 的（12 核为 4×3，64 核为 8×8）。放不下时网格区自己滚动。
- 详情页放进 `ScrollView`，统计项用 `stats-panel.slint` 的 `StatsPanel`（按宽度自动定列数）；截图步骤固定包含 760×480 和 900×600 两种尺寸。
- 逐核曲线和 CPU 统计只在 CPU 被选中、并且处于“逻辑处理器”视图时才计算。基准速度取 `CallNtPowerInformation(ProcessorInformation)` 的 MaxMhz：注册表 `~MHz` 是 3893，会显示成 3.89 GHz。超过 64 个逻辑处理器（多个处理器组）时，`SystemProcessorPerformanceInformation` 只返回当前组的核，这是已知限制。
- 内存：已缓存 = 备用 + 已修改（`SystemMemoryListInformation`，class 80）；已压缩取 Memory Compression 进程的工作集。SMBIOS Type 17 通过 `GetSystemFirmwareTable('RSMB')` 在首个快照之后读一次，不用 WMI。
- 分组视图：“应用”的判定是 `visible && (WS_EX_APPWINDOW || (!owner && !toolwindow)) && 有标题 && !cloaked`。`DWMWA_CLOAKED=2` 同时覆盖挂起的 UWP 和其他虚拟桌面上的窗口（#40 起用 `IVirtualDesktopManager` 区分，见上）。EnumWindows 只在分组视图下每次采样运行，单次约 0.08 ms。应用行显示整棵进程树的合计。
- 分组标题行的 `index` 是 `HEADER_INDEX`（`usize::MAX`）：凡是按行号去取 `snapshot.processes` 的代码，都必须先经过 `ViewRow::process()`，否则会越界 panic，而且 git 合并时不会报冲突。
- 测试里 `cmd /c ping -t` 只结束 cmd 会留下孤儿 ping：直接启动 ping.exe，或者用 Drop 守卫结束整棵进程树。
- 合并流程：仓库没有必需检查，`gh pr merge --auto` 会立即合并，所以要等 CI 跑完再手动合并。agent 没有 force push 权限，feature 分支用合并提交并入 main，不用 rebase。堆叠的 PR 在基础 PR squash 合并后会出现 add/add 冲突；先确认 `git diff <基础 PR head> origin/main` 为空，再取堆叠分支一侧解决冲突。

## 2026-10-28 00:20 进程图标
- `slint::Image` 的 `PartialEq` 按像素缓冲指针比较，空 `Image` 彼此不相等：放进 `ProcRow` 会让“行未变化则跳过”的判断全部失效。空图标用同一个 1×1 透明图的克隆占位。
- `slint::Image` 不是 Send：后台线程只传 `SharedPixelBuffer`，界面线程再转 Image。
- 图标尺寸按 `16 × scale_factor` 取物理像素（200% 缩放取 32px）；`PrivateExtractIconsW` 可按任意尺寸取。
- 不要用 `SHGetFileInfoW` 取图标：它会把整套外壳 DLL（windows.storage、thumbcache、PROPSYS 等）拉进进程，常驻工作集约 +6.8 MB；并发调用还会失败（测试并行跑 10 次失败 6 次）。没有图标资源的 exe（svchost、RuntimeBroker）和拿不到路径的 System 一律用默认图标：`imageres.dll` 资源 ID 15（`PrivateExtractIconsW` 传索引 -15，就是外壳显示的蓝色应用窗口图标），再退 `IDI_APPLICATION`；每个尺寸只取一次并缓存。改后相对 main 的 release 常驻内存约 +1.7 MB 工作集、+1.0 MB 私有（剩余的 WindowsCodecs 来自 PNG 压缩图标的解码）。
- `IconCache::resolved` 只接受 `Source::Resolving` 的进程：DPI 变化 `reset()` 后新旧两次 Resolve 都会回来，重复结果必须忽略，否则引用计数多加一次、条目永远释放不掉。
- `procinfo.rs` 的路径读取只用 `PROCESS_QUERY_LIMITED_INFORMATION`，System（PID 4）等取不到时为 `None`；#4、#17 复用该模块。

## 2026-10-27 23:59 右键菜单
- Windows 上 Slint 的 `ContextMenuArea` 走 muda 的原生弹出菜单（`TrackPopupMenu`，模态）：`take_snapshot` 截不到，`--screenshot` 里触发会阻塞事件循环。菜单截图用 `scripts/menu-screenshots.ps1`（应用 `--menu-shot` 自己右键，脚本按类名 `#32768` 找菜单窗口并 `PrintWindow`，脚本须声明 DPI 感知）。原生菜单不随应用深浅色变化。
- 行内 `TouchArea` 会吞掉右键，`ContextMenuArea` 收不到：在 `pointer-event` 里右键按下时先选中行、让 Rust 读取路径/命令行设置 `ProcessMenu` 的可用状态，再调用 `menu.show(...)`。菜单项 `enabled` 绑定全局属性，弹出时求值。
- 命令行用 `NtQueryInformationProcess(ProcessCommandLineInformation=60)` + 受限查询权限；受保护进程（System）返回 `None`。`std::process::Command` 会给程序名加引号，测试断言要带引号。
- 交互自动化用 PostMessage 发 `WM_RBUTTONDOWN/UP`，不动用户的真实鼠标；菜单用 `WM_CANCELMODE` 关闭。真实光标停在窗口内时，菜单关闭后系统会补发一次 `WM_MOUSEMOVE` 到光标所在位置，会干扰后续的模拟点击，所以测试前要把窗口移到光标不在的位置。

## 2026-10-27 23:10 设置持久化与窗口恢复
- 窗口显示前调用 `set_size` 会被 `preferred-width/height` 覆盖：改为把保存的大小写入 `initial-width/height` 属性并绑定到 preferred。大小按逻辑像素保存（不同缩放的显示器上视觉大小一致），位置按物理像素，`set_position` 在显示前有效。
- 优雅退出靠 `on_close_requested` 保存（WM_CLOSE 会触发）；以管理员身份重启前也要先保存。
- 设置文件用 serde_json 的 `Value` 逐项解析（不用 derive），未知字段原样保留；release exe 约增加 105 KB。
- 列宽拖动：右对齐布局下列的左边缘会随宽度移动，用 `pressed-x - mouse-x` 每次修正；截图模式可用 `Window::dispatch_event` 模拟真实指针拖动。

## 2026-10-27 18:30 Slint 1.18 踩坑
- Path 的 `viewbox` 是等比缩放，宽高比不同会被压扁：曲线路径按控件实际像素尺寸生成，并在尺寸变化时重算（`graph-resized`）。
- software 渲染器不支持 `set_rendering_notifier`，首帧耗时只能在事件循环空闲时打点近似。
- 自定义 Rectangle 子组件不能声明 `color` 属性（与内置的已弃用属性冲突），改名为 `tint`。
- 微软雅黑没有 ▾▴▸ 等字形会显示方块，箭头用 Path 画（`Chevron`）。
- 视觉验收用 `--screenshot` 离屏截图（`take_snapshot`），不占用桌面，用户在玩游戏时也能跑。

## 2026-10-27 18:30 采样
- 首次采样没有基线，CPU/磁盘/网络为 0，不写入曲线历史，否则曲线开头会有一条竖线。
- 已退出但仍被引用的进程对象打开时也报拒绝访问：结束进程失败时要用受限句柄复核创建时间和退出码，区分“权限不足”和“已退出”。
- 自动化测试只结束自己创建的进程，绝不碰 csrss 等关键进程（提权运行会直接蓝屏）。
