# 开发经验

## 2026-10-27 23:59 右键菜单
- Windows 上 Slint 的 `ContextMenuArea` 走 muda 的原生弹出菜单（`TrackPopupMenu`，模态）：`take_snapshot` 截不到，`--screenshot` 里触发会阻塞事件循环。菜单截图用 `scripts/menu-screenshots.ps1`（应用 `--menu-shot` 自己右键，脚本按类名 `#32768` 找菜单窗口并 `PrintWindow`，脚本须声明 DPI 感知）。原生菜单不随应用深浅色变化。
- 行内 `TouchArea` 会吞掉右键，`ContextMenuArea` 收不到：在 `pointer-event` 里右键按下时先选中行、让 Rust 读取路径/命令行设置 `ProcessMenu` 的可用状态，再调用 `menu.show(...)`。菜单项 `enabled` 绑定全局属性，弹出时求值。
- 命令行用 `NtQueryInformationProcess(ProcessCommandLineInformation=60)` + 受限查询权限；受保护进程（System）返回 `None`。`std::process::Command` 会给程序名加引号，测试断言要带引号。

## 2026-10-27 23:40 进程图标
- `slint::Image` 的 `PartialEq` 按像素缓冲指针比较，空 `Image` 彼此不相等：放进 `ProcRow` 会让“行未变化则跳过”的判断全部失效。空图标用同一个 1×1 透明图的克隆占位。
- `slint::Image` 不是 Send：后台线程只传 `SharedPixelBuffer`，界面线程再转 Image。
- 图标尺寸按 `16 × scale_factor` 取物理像素（200% 缩放取 32px）；`PrivateExtractIconsW` 可按任意尺寸取，没有图标资源的 exe（如 rustc、svchost）回退外壳通用图标，与原生任务管理器一致。
- `procinfo.rs` 的路径读取只用 `PROCESS_QUERY_LIMITED_INFORMATION`，System（PID 4）等取不到时为 `None`；#4、#17 复用该模块。

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
