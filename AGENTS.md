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
- 测试：`cargo test -p tml-core -p tml-app`

## Agent skills

### Issue tracker

Issue 记录在 GitHub Issues（`djp440/TaskManagerLite`），用 `gh` CLI 操作。See `docs/agents/issue-tracker.md`.

### Triage labels

使用默认的五个 triage 标签：`needs-triage`、`needs-info`、`ready-for-agent`、`ready-for-human`、`wontfix`。See `docs/agents/triage-labels.md`.

### Domain docs

单上下文：根目录一个 `GLOSSARY.md`，ADR 放在 `docs/adr/`。See `docs/agents/domain.md`.
