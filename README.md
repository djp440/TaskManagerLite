# TaskManagerLite

**简体中文** | [English](README.en.md)

用 Rust + [Slint](https://slint.dev) 重写的 Windows 任务管理器：冷启动快、常驻开销低、界面美观。

![进程列表（深色）](docs/images/processes-dark.png)

| 性能概览 | 服务 |
| --- | --- |
| ![性能概览 CPU（深色）](docs/images/performance-cpu-dark.png) | ![服务页（深色）](docs/images/services-dark.png) |

## 功能

- **进程**：排序、搜索、进程图标；平铺、进程树、按“应用 / 后台进程”分组三种视图；可选“详细信息”列（列选择、横向滚动）；结束进程或整棵进程树。
- **右键菜单**：打开文件位置、复制路径与命令行、查看属性、在线搜索。
- **性能**：CPU（含逻辑处理器视图，支持多个处理器组）、内存（物理内存组成、提交信息、插槽与速度）、按物理磁盘（含分区信息）、按网卡（型号、虚拟网卡）、GPU（型号、显存曲线）。可选用 ETW 统计更准确的进程磁盘速率（需管理员权限，默认关闭）。
- **启动项**：枚举 Run / Run32（HKCU、HKLM）与启动文件夹，启用或禁用的状态与原生任务管理器互通；系统级项目通过 UAC 子进程修改。
- **服务**：列出名称、显示名称、描述、状态、启动类型、PID、组；启动、停止、重启（需要管理员权限，失败时给出原因）；一键跳转到进程页对应 PID。
- **迷你模式**：只显示资源卡片的置顶小窗口。
- **设置**：开机启动、深浅色与跟随系统主题、关闭时最小化到托盘、后台继续采样、窗口置顶、接管 Ctrl+Shift+Esc（默认关闭）。
- **按需提权**：默认以普通权限启动，不弹 UAC；需要查看或结束受保护的进程时再以管理员身份重启。
- **轻量常驻**：采样间隔可调（0.5 / 1 / 2 秒或暂停），窗口不可见时停止采样；只允许运行一个实例。
- **设置持久化**：窗口位置与大小、列宽等保存在 `%APPDATA%\TaskManagerLite\settings.json`。

<details>
<summary>更多截图</summary>

| 启动项 | 迷你模式 | 设置 |
| --- | --- | --- |
| ![启动项（深色）](docs/images/startup-dark.png) | ![迷你模式（深色）](docs/images/mini-dark.png) | ![设置页（深色）](docs/images/settings-dark.png) |

| GPU | 浅色主题 |
| --- | --- |
| ![GPU（深色）](docs/images/performance-gpu-dark.png) | ![进程列表（浅色）](docs/images/processes-light.png) |

</details>

启动耗时与常驻开销的实测数据见 [ADR 0002](docs/adr/0002-software-renderer-default.md)。

## 下载

从 [GitHub Releases](https://github.com/djp440/TaskManagerLite/releases) 下载 `TaskManagerLite-<版本>.exe`，同目录下的 `.sha256` 文件可用于校验。无需安装，直接运行。

## 从源码构建

需要 Windows 和 stable Rust 工具链。

```sh
# 默认 software 渲染
cargo build -p tml-app --release

# femtovg（OpenGL）渲染
cargo build -p tml-app --release --no-default-features --features renderer-femtovg

# 测试
cargo test -p tml-core -p tml-app
```

产物为 `target/release/TaskManagerLite.exe`。

## 项目结构

- `crates/core`（`tml-core`）：采集进程快照与系统级指标、构建进程树、结束进程、枚举启动项与服务，纯 Rust，可脱离界面单测。
- `crates/app`（`tml-app`）：Slint 界面与采样线程。
- `scripts/`：启动耗时、常驻开销测量、截图与端到端验收脚本。
- `docs/adr/`：架构决策记录；`docs/checklist/`：人工验收清单。
- `GLOSSARY.md`：领域术语。

## 许可证

本项目以 [MIT 许可证](LICENSE) 发布。

界面基于 Slint 构建，按 [Slint Royalty-free License 2.0](https://github.com/slint-ui/slint/blob/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md) 使用：

[![Made with Slint](https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-whitebg.png)](https://slint.dev)
