# 安装包人工验收清单（2026-10-29）

范围：Inno Setup 安装包与接管任务管理器（ADR 0005）。

安装包：`pwsh scripts/build-installer.ps1 -Build` 生成 `target/installer/TaskManagerLite-<版本>-setup.exe`（构建前先退出从 `target/release` 运行的实例）。

已自动验收（无需人工）：`scripts/test-installer.ps1`（按用户安装到临时目录：实例运行时拒绝安装、占用时不覆盖、勾选接管写入登记、覆盖安装保留登记、卸载撤销登记 / 开机启动 / 文件 / 快捷方式）；`scripts/winget-manifest.ps1` 的 `winget validate`。下面只列需要 UAC、HKLM 或真实快捷键的项目。

## 待验收

- [x] **1. 安装向导与 UAC**
  - 操作：双击安装包，在 UAC 中点“是”；逐页查看向导。
  - 预期：界面是简体中文；默认目录是 `C:\Program Files\TaskManagerLite`；“替换系统任务管理器”和“创建桌面快捷方式”默认都不勾。
  - 反馈：通过（2026-10-29）

- [x] **2. 安装时勾选接管**
  - 操作：勾选“替换系统任务管理器”完成安装；按 Ctrl+Shift+Esc；打开设置页看接管开关。
  - 预期：安装过程中没有第二次 UAC；Ctrl+Shift+Esc 打开 TaskManagerLite；设置页开关为开，没有“位置可被其他用户改写”的报错。
  - 反馈：通过（2026-10-29）

- [x] **3. 完成页的“启动”不提权**
  - 操作：在完成页保留“启动 TaskManagerLite”勾选，点完成。
  - 预期：程序启动，顶栏仍显示“以管理员身份重启”（说明是普通权限运行）。
  - 反馈：通过（2026-10-29）

- [x] **4. 运行中升级**
  - 操作：开启“关闭时最小化到托盘”并让程序常驻托盘，再运行一次安装包。
  - 预期：安装程序提示 TaskManagerLite 正在运行，退出程序后点“确定”可继续；完成后 Ctrl+Shift+Esc 仍然有效。
  - 反馈：通过（2026-10-29）

- [x] **5. 卸载还原**
  - 操作：在“设置 → 应用 → 安装的应用”中卸载 TaskManagerLite；然后按 Ctrl+Shift+Esc；检查开始菜单。
  - 预期：卸载完成后 Ctrl+Shift+Esc 打开系统自带任务管理器（不是报找不到文件）；开始菜单里没有 TaskManagerLite；`C:\Program Files\TaskManagerLite` 已删除。
  - 反馈：通过（2026-10-29）

- [x] **6. winget 本地清单安装（可选）**
  - 操作：管理员终端执行 `winget settings --enable LocalManifestFiles`；发布新版本后运行 `pwsh scripts/winget-manifest.ps1`，再 `winget install --manifest target\winget\<版本>`。
  - 预期：静默安装到 Program Files；`winget list TaskManagerLite` 能识别到；`winget uninstall TaskManagerLite` 能卸载。
  - 反馈：通过（2026-10-29）
