# TaskManagerLite

用 Rust + Slint 重写的 Windows 任务管理器，目标是冷启动快、常驻开销低、界面美观。MVP 包含进程列表和性能概览。

## 项目结构

- `crates/core`（`tml-core`）：采集进程快照与系统级指标、构建进程树、结束进程。纯 Rust，可脱离界面单测。
  核心库不返回面向界面的显示文案：状态、类型、来源用枚举，错误用带原因与系统错误码的结构化类型（如 `ServiceError`、`StartupError`、`IfeoError`、`ShellError`）；中文文案集中在 `crates/app/src/texts.rs` 按枚举映射（本地化接入点）。
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
- 键盘快捷键（#71）：Ctrl+F 聚焦当前页搜索框（进程 / 启动项 / 服务页各有一个，性能、设置页跳到进程页的搜索框）、Esc（搜索框内）清空并回到列表、Ctrl+1…5 切页（进程 / 性能 / 启动项 / 服务 / 设置）、F5 立即刷新当前页；说明写在设置页底部“键盘快捷键”。端到端验收：`pwsh scripts/test-shortcuts.ps1`（自己启动 `--allow-multiple` 实例，用 SendInput 发真实按键，读日志里的“界面事件”等行核对，并保存截图；运行期间不要动键盘鼠标，需交互式桌面）
- 导出 CSV 与复制所选行（#82）：进程页右键行菜单“复制所选行”（Ctrl+C，搜索框有焦点时留给文本框）、“导出为 CSV…”，表头右键菜单也有“导出为 CSV…”；性能页左栏底部“导出当前快照…”（顶栏已满，不放顶栏）。进程导出按当前视图、可见列、排序，单元格文字与界面一致（`controller/export.rs`，CPU/内存/磁盘文字与界面共用 `basic_texts`，基本列标题来自 Slint `Columns.title-*`），树形/分组视图展开全部后代并多“层级”“分组”列；含用户名/映像路径/命令行列时先弹敏感列确认（ConfirmDialog 的 kind 3）；保存对话框在 `file_dialog::pick_save_file`（后台线程），写文件在 core 的 `csv.rs`（UTF-8 BOM、RFC 4180、CRLF，不引第三方库）；日志只记行数、列数、结果、错误码。测试钩子 `--export-csv <process|performance>=<路径>`（不弹对话框，直接写文件后退出，读设置同正常启动）、`--menu-action copy-row:<pid>[:group]`。端到端验收：`pwsh scripts/test-export-csv.ps1`（需交互式桌面，含真实鼠标键盘走保存对话框；`-SkipKeyboard` 跳过真实输入部分），人工项见 `docs/checklist/issue-82.md`
- 进程优先级与 CPU 亲和性（#66）：进程页右键菜单“设置优先级”子菜单（实时 / 高 / 高于正常 / 正常 / 低于正常 / 低，当前级别打勾，读不到则不打勾）与“设置亲和性…”（Slint 对话框，按逻辑处理器勾选、全选 / 全不选；超过 64 个逻辑处理器只支持进程所在的当前处理器组，对话框里提示）。选“实时”先弹确认框（ConfirmDialog 的 kind 4）；未提权时系统会把“实时”静默降为“高”，状态栏会如实说明。受保护进程等打不开可写句柄的，两项在菜单弹出前就置灰。实现在 core 的 `procctl.rs`（`PriorityClass`、`ProcessControlError`、`control_state`、`set_priority`、`set_affinity`；失败用结构化错误，文案在 `texts.rs::process_control_error`）与 `controller/process_control.rs`；修改后补采一次，“优先级”列下一次采样内更新；日志只记 PID、结果与错误码。定向测试：`cargo test -p tml-core procctl --lib`、`cargo test -p tml-app process_control`；端到端验收：`pwsh scripts/test-process-priority.ps1`（自己启动 ping 子进程，用 `--menu-action set-priority:<pid>:<优先级键>` / `set-affinity:<pid>:<十六进制掩码>` 改，再用 .NET 回读；不依赖 SendInput，需要先 `cargo build -p tml-app --release`）；亲和性对话框截图：`--menu-action affinity-shot:<pid>` 配合环境变量 `TML_AFFINITY_SHOT=<bmp 路径>`；人工项见 `docs/checklist/issue-66.md`。
- 进程效率模式（EcoQoS，#68）：进程页右键菜单“效率模式”（可勾选）与“详细信息”可选列“效率模式”（默认不显示；只有显示该列时采样器才按进程读状态，`Sampler::track_eco_mode`）。开启 = `SetProcessInformation(ProcessPowerThrottling, EXECUTION_SPEED)` + 优先级“低”，关闭 = 显式禁止节流（Control=SPEED、State=0；Control=0 是空操作）并按联动策略恢复优先级；读回用 `NtQueryInformationProcess(77)`（`GetProcessInformation` 在 Win10 上恒返回 87）。系统低于 Windows 10 1709（build 16299，`RtlGetVersion`）时菜单项与列隐藏；系统关键进程与本程序自身置灰；应用行作用于该应用及其全部后代（口径同结束进程树，explorer.exe 只作用自身），多于 1 个进程先走 ConfirmDialog kind 5。实现在 core 的 `procctl.rs`（`set_eco_mode`、`eco_mode`、`eco_supported`）与 `controller/process_control.rs`（优先级记忆 `EcoState`、`eco_restore_target`、`eco_targets`）。定向测试：`cargo test -p tml-core procctl --lib`、`cargo test -p tml-app process_control`；端到端：`pwsh scripts/test-ecoqos.ps1`（`--menu-action eco-mode:<pid>:<on|off>`，不依赖 SendInput，需要先 `cargo build -p tml-app --release`）；人工项见 `docs/checklist/issue-68.md`。
- 挂起与恢复进程（#67）：进程页右键菜单“挂起”“恢复”（`NtSuspendProcess` / `NtResumeProcess`，动态绑定在 `crates/core/src/procctl.rs`：`suspend_process`、`resume_process`、`still_suspended`）。“已挂起”直接取自采样快照（`Counters::suspended`：线程全部处于 Waiting + Suspended 等待原因），零额外系统调用；挂起是计数式的，已挂起的进程“挂起”置灰、未挂起的“恢复”置灰；关键进程或一次作用于多个进程（应用行 = 应用及全部后代，口径复用 `process_control::app_row_keys`）先弹 ConfirmDialog（kind 6），PID 0 / 4 与本程序自身置灰；探测权限见 `ControlState::suspend`。真正退出（关闭按钮、托盘“退出”、迷你窗口退出；不含隐藏到托盘、最小化）前，若本程序挂起且仍保持挂起的进程还在（记录 `ProcessKey`，退出时用一次进程枚举核对），弹 ConfirmDialog（kind 7）提示一次，退出后不自动恢复（`controller/suspend.rs` 的 `try_exit`）。测试钩子 `--menu-action suspend|resume|suspend-ask|exit-prompt|exit-prompt-twice|exit-noprompt:<pid>[:group]`。端到端验收：`pwsh scripts/test-suspend.ps1`（只挂起自己启动的进程并回读线程 `WaitReason`，不用 SendInput，结束前先恢复再结束），人工项见 `docs/checklist/issue-67.md`。
- 进程与服务互相跳转（#85）：进程页右键菜单“转到服务”（承载运行中服务时可用，否则置灰；点击后切到服务页，按该 PID 过滤并选中第一项，顶部“正在按进程 … 过滤”提示条，搜索 / 点击“清除过滤” / 离开服务页时清除；枚举异步，等本次枚举完成后才选中，过滤对旧列表也生效所以不会闪出完整列表）与“详细信息”可选列“服务”（默认不显示，口径见 `GLOSSARY.md` “宿主进程”，界面与 CSV 导出共用 `details::services_text`）。数据来源：core 的 `services::hosted_services()`（只做一次 `EnumServicesStatusExW`，不读配置，约 1 ms）与 `host_map`；“服务”列只有显示时采样器才枚举（`Sampler::track_service_hosts`，`Command::Services`，每 5 秒一次 `SERVICE_HOSTS_TTL`，关闭即清缓存），右键菜单弹出与点击时各现枚举一次，常驻开销不变。过滤状态在 Slint 的 `ServiceData.filter-pid`，逻辑在 `controller/services_page.rs`（`goto_services_of`、`visible_services`），菜单在 `controller/process_menu.rs`。定向测试：`cargo test -p tml-core services --lib`、`cargo test -p tml-app services_page`；端到端：`pwsh scripts/test-goto-service.ps1`（`--menu-action goto-service:<pid>`，设 `TML_GOTO_SHOT=<bmp 路径>` 保存服务页截图；对照 Win32_Service，不依赖 SendInput，需要先 `cargo build -p tml-app --release`）；人工项见 `docs/checklist/issue-85.md`。
- 应用窗口操作（#70）：分组视图应用行的右键菜单“切换到”“最大化”“最小化”“还原”（普通进程行、进程树 / 平铺视图下整组隐藏）。窗口句柄由 core 的 `apps.rs::app_windows` 在判定“应用”时顺带记下（`Sample::app_windows`，主窗口见 `GLOSSARY.md`；UWP 取外层 `ApplicationFrameWindow`），实现在 core 的 `winops.rs`（`probe_window`、`apply_window_op`、`WindowError`、`needs_elevation`；失败用结构化错误，文案在 `texts.rs::window_*`）与 `controller/window_ops.rs`。菜单弹出前读一次 `IsIconic/IsZoomed` 置灰已处于目标状态的项；目标进程令牌完整性高于本程序（或令牌被拒绝访问，提权应用的典型表现）时四项都置灰并写“需要提权”。“切换到”先还原最小化的窗口再 `SetForegroundWindow`，被前台锁定拒绝时退化为 `FlashWindowEx` 闪烁任务栏并在状态栏说明。日志只记 PID、结果与错误码。定向测试：`cargo test -p tml-core winops --lib`、`cargo test -p tml-app window_ops`；端到端：`pwsh scripts/test-window-ops.ps1`（自己启动记事本，`--menu-action window-activate|window-maximize|window-minimize|window-restore:<pid>[:group]`，Win32 回读窗口状态；不依赖 SendInput，需要先 `cargo build -p tml-app --release`）；人工项见 `docs/checklist/issue-70.md`。
- 运行新任务（#69）：Ctrl+N 或侧边栏底部“运行新任务”（动作项，不画选中条）打开 Slint 对话框，输入程序、文件夹、文档或网址，可选“以管理员权限创建此任务”（`runas`，会弹 UAC）、“浏览…”（`GetOpenFileNameW`，后台线程、主窗口为所有者）；最近 10 条输入存 `settings.json` 的 `run_history`（缺字段按空处理）。命令拆分与环境变量展开在 core 的 `runtask.rs`（`parse_command`、`run_task`，返回结构化的 `RunError`），文案在 `texts.rs::run_error`；启动失败或 UAC 取消时在对话框里显示原因且不关闭。端到端验收：`pwsh scripts/test-run-task.ps1`（`--allow-multiple` 实例 + SendInput，只清理自己启动的记事本；不覆盖 UAC、网址、explorer，见 `docs/checklist/issue-69.md`；中文输入法下第一下回车只是上屏，脚本会补按一次）
- 页面导航在左侧边栏（进程 / 性能 / 启动项 / 服务，设置在底部）：窗口宽度达 900 且未折叠时图标加文字（160px），否则只有图标（48px）；顶部菜单按钮折叠，状态存 `settings.json` 的 `sidebar_collapsed`。
- 设置页（左侧边栏底部进入）：开机启动写 `HKCU\...\Run`（值 `"<exe>" --autostart`，状态以注册表为准，不写设置文件；环境变量 `TML_AUTOSTART_TEST_KEY` 改写到 `HKCU\Software\TaskManagerLite-test\...` 测试键）。`--autostart` 启动时：开启了 `close_to_tray` 就只建托盘图标、不显示窗口，否则最小化启动。端到端验收：`pwsh scripts/test-autostart.ps1`（隔离单实例名，不碰真实 Run 键）
- 设置持久化：`%APPDATA%\TaskManagerLite\settings.json`（环境变量 `TML_SETTINGS_DIR` 可覆盖目录；`--screenshot`、`--measure-startup` 不读写）。端到端验收：`pwsh scripts/e2e-settings.ps1`（脚本用 `--allow-multiple` 启动，不受已运行实例影响）
- 进程级 GPU 占用列（“详细信息”可选列 `gpu`，只在显示该列时才按 PID 聚合 `GPU Engine` 计数器，口径见 `GLOSSARY.md`）：探针 `cargo run -p tml-core --release --example process_gpu_probe -- --secs 10 [--pid N] [--top 8]`（CSV `t,pid,name,gpu`，无读数为 `none`）；测显示该列的常驻开销 `pwsh scripts/measure-idle.ps1 -Gpu`；人工验收见 `docs/checklist/issue-64.md`。
- ETW 磁盘速率（设置项 `accurate_disk_io`，默认关闭，见 ADR-0004）端到端验收（**需提权终端**，未提权直接退出）：`pwsh scripts/test-etw-disk.ps1`；探针：`cargo run -p tml-core --release --example disk_probe -- --secs 20 [--no-etw] [--pid N]`（输出每进程磁盘速率 CSV，`source` 列区分 `etw`/`approx`）
- 接管 Ctrl+Shift+Esc（IFEO，见 ADR 0003）：开关 `--register-taskmgr` / `--unregister-taskmgr` / `--taskmgr-status`（可加 `--force`）；端到端验收 `pwsh scripts/test-taskmgr-ifeo.ps1`（环境变量 `TML_IFEO_TEST_KEY` 改写到 HKCU 测试键，不碰 HKLM、不提权）
- 测试：`cargo test -p tml-core -p tml-app`
- 启动项页：进入页面时后台枚举 Run / Run32（HKCU、HKLM）和两个启动文件夹，状态写入 StartupApproved，系统级项通过 UAC 子进程修改；点击列表项或重新进入页面会重新枚举，以同步外部修改（页面没有刷新按钮）。定向测试：`cargo test -p tml-core startup --lib`；人工互通验收见 `docs/checklist/issue-25.md`。
- 服务页：进入页面时后台枚举（`EnumServicesStatusExW`，启动类型/描述/组按服务名缓存，进入页面时清缓存），页面可见时随每次完整采样刷新；启动/停止/重启未提权时经 `--service-control` UAC 子进程执行。定向测试：`cargo test -p tml-core services --lib`；普通权限端到端：`pwsh scripts/test-services.ps1`（用 `--service-action <start|stop|restart|goto>:<服务名>` 启动自己的实例，不弹 UAC，不真的启停服务）；人工验收见 `docs/checklist/issue-26.md`。
- “应用”判定的真实桌面验收（会启动记事本、计算器并测量枚举耗时）：`pwsh scripts/test-apps.ps1`；`measure-idle.ps1 -View group` 测分组视图的常驻开销，`-Mini` 测迷你模式的常驻开销
- 内存页数据核对（与 WMI、`\Memory\*` 计数器对比插槽、速度、可用、提交、分页池等）：`pwsh scripts/verify-memory.ps1`
- CI：推送到 main 或开 PR 时自动跑测试与 release 构建（`.github/workflows/ci.yml`）
- 安装包（Inno Setup，见 ADR 0005）：`pwsh scripts/build-installer.ps1 [-Build]` → `target/installer/TaskManagerLite-<版本>-setup.exe`（本机需 `winget install JRSoftware.InnoSetup`）；端到端验收 `pwsh scripts/test-installer.ps1`（按用户安装到临时目录，不提权，只读写 HKCU 测试键）
- winget 清单：`pwsh scripts/winget-manifest.ps1 [-Version x.y.z] [-Installer <本地安装包>]` 生成到 `target/winget/<版本>/` 并校验；只生成不提交
- 发布：先改 `Cargo.toml` 的 workspace 版本，再推送同名 tag（如 `v0.1.0`，带 `-` 的视为预发布），`release.yml` 会构建并上传安装包与 sha256 到 GitHub Releases

## Agent skills

### Issue tracker

Issue 记录在 GitHub Issues（`djp440/TaskManagerLite`），用 `gh` CLI 操作。See `docs/agents/issue-tracker.md`.

### Triage labels

使用默认的五个 triage 标签：`needs-triage`、`needs-info`、`ready-for-agent`、`ready-for-human`、`wontfix`。See `docs/agents/triage-labels.md`.

### Domain docs

单上下文：根目录一个 `GLOSSARY.md`，ADR 放在 `docs/adr/`。See `docs/agents/domain.md`.
