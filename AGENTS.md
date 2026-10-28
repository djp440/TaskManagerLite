# TaskManagerLite

用 Rust + Slint 重写的 Windows 任务管理器，目标是冷启动快、常驻开销低、界面美观。MVP 包含进程列表和性能概览。

## 项目结构

- `crates/core`（`tml-core`）：采集进程快照与系统级指标、构建进程树、结束进程。纯 Rust，可脱离界面单测。
- `crates/app`（`tml-app`，产物 `TaskManagerLite.exe`）：Slint 界面与采样线程。
- `scripts/`：启动耗时、常驻开销测量与截图脚本。
- `GLOSSARY.md`：领域术语，命名以它为准。
- `docs/adr/`：架构决策记录。
- `docs/checklist/`：人工验收清单。

## 常用命令

- 构建（默认 software 渲染）：`cargo build -p tml-app --release`
- 构建（femtovg 渲染）：`cargo build -p tml-app --release --no-default-features --features renderer-femtovg`
- 测启动耗时：`pwsh scripts/measure-startup.ps1`；测常驻开销：`pwsh scripts/measure-idle.ps1`；生成视觉验收截图：`pwsh scripts/screenshots.ps1`（离屏，输出到 `docs/checklist/media/<日期>/`）
- 右键菜单验收截图：`pwsh scripts/menu-screenshots.ps1 -Out <目录>`（原生菜单，菜单会在屏幕上短暂出现；用 `--menu-shot` 参数启动应用）
- 右键菜单动作端到端验收：`pwsh scripts/test-menu-actions.ps1`（自己启动的子进程；用 `--menu-action <动作>:<pid>` 触发动作，检查资源管理器窗口、属性对话框与剪贴板；会临时改写剪贴板并在结束时还原）
- 单实例限制的 Win32 自动化验收：`pwsh scripts/test-single-instance.ps1`（需先关闭已有实例）；开发时想多开用 `--allow-multiple`（`--measure-startup`、`--screenshot` 同样不受限制）
- 后台继续采样（设置字段 `background_sampling`）端到端验收：`pwsh scripts/test-background-sampling.ps1`（最小化自己启动的实例，核对曲线连续性并测最小化期间 CPU）
- 托盘常驻（设置字段 `close_to_tray`）端到端验收：`pwsh scripts/test-tray.ps1`（用 `TML_INSTANCE_ID` 隔离单实例对象名，机器上已有真实实例也可运行；向托盘消息窗口发消息模拟单击/右键/菜单命令，验证隐藏/显示、WM_CLOSE 后常驻、再次启动唤起、隐藏期间 CPU；截图输出到 `-Out`）
- 设置持久化：`%APPDATA%\TaskManagerLite\settings.json`（环境变量 `TML_SETTINGS_DIR` 可覆盖目录；`--screenshot`、`--measure-startup` 不读写）。端到端验收：`pwsh scripts/e2e-settings.ps1`（脚本用 `--allow-multiple` 启动，不受已运行实例影响）
- 接管 Ctrl+Shift+Esc（IFEO，见 ADR 0003）：开关 `--register-taskmgr` / `--unregister-taskmgr` / `--taskmgr-status`（可加 `--force`）；端到端验收 `pwsh scripts/test-taskmgr-ifeo.ps1`（环境变量 `TML_IFEO_TEST_KEY` 改写到 HKCU 测试键，不碰 HKLM、不提权）
- 测试：`cargo test -p tml-core -p tml-app`
- “应用”判定的真实桌面验收（会启动记事本、计算器并测量枚举耗时）：`pwsh scripts/test-apps.ps1`；`measure-idle.ps1 -View group` 测分组视图的常驻开销
- 内存页数据核对（与 WMI、`\Memory\*` 计数器对比插槽、速度、可用、提交、分页池等）：`pwsh scripts/verify-memory.ps1`
- CI：推送到 main 或开 PR 时自动跑测试与 release 构建（`.github/workflows/ci.yml`）
- 发布：先改 `Cargo.toml` 的 workspace 版本，再推送同名 tag（如 `v0.1.0`，带 `-` 的视为预发布），`release.yml` 会构建并上传 exe 与 sha256 到 GitHub Releases

## Agent skills

### Issue tracker

Issue 记录在 GitHub Issues（`djp440/TaskManagerLite`），用 `gh` CLI 操作。See `docs/agents/issue-tracker.md`.

### Triage labels

使用默认的五个 triage 标签：`needs-triage`、`needs-info`、`ready-for-agent`、`ready-for-human`、`wontfix`。See `docs/agents/triage-labels.md`.

### Domain docs

单上下文：根目录一个 `GLOSSARY.md`，ADR 放在 `docs/adr/`。See `docs/agents/domain.md`.
