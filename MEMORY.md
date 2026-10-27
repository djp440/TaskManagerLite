# 开发经验

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
