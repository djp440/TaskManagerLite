# 0005 用 Inno Setup 安装包分发，winget 清单随后提交

状态：已采纳（2026-10-29）。

## 背景

接管 Ctrl+Shift+Esc（ADR 0003）在登记前检查 exe 及其目录是否可被 Everyone / Authenticated Users / Users 改写。绿色版 exe 常被放在 `D:\Tools` 这类盘符根下新建的目录，这些目录默认授予 Authenticated Users 修改权限，于是检查不通过，用户只能自己把 exe 挪到 Program Files（需要管理员权限的手工操作）。放宽检查等于给 IFEO 留一条可被普通用户改写的代码执行入口，不可取。

## 决定

- 发布物从单个 exe 改为 Inno Setup 安装包 `TaskManagerLite-<版本>-setup.exe`（`installer/TaskManagerLite.iss`，`scripts/build-installer.ps1` 编译），不再上传绿色版 exe。
- 默认按机器安装到 `{autopf}\TaskManagerLite`（Program Files，仅管理员可写）。命令行 `/CURRENTUSER` 可按用户安装到 `%LOCALAPPDATA%\Programs`（同样能通过位置检查），主要供自动化测试免提权使用。
- 安装向导提供“替换系统任务管理器”任务（默认不勾）。安装程序已提权，直接调用 `TaskManagerLite.exe --register-taskmgr`，不再弹 UAC；被其他程序占用（退出码 3）时询问是否 `--force`，静默安装默认不覆盖。设置文件里的 `replace_taskmgr` 意愿不写（安装程序是提权进程，可能不是登录用户），设置页本来就以注册表实际状态为准。
- 卸载时删除文件之前运行 `--unregister-taskmgr`（不带 `--force`，只删指向本程序或已失效的登记），避免 exe 被删后 Ctrl+Shift+Esc 失灵；同时删除 HKCU 下指向本安装位置的开机启动登记及其 StartupApproved 值。设置文件与日志保留。
- `AppMutex` 用主实例的单实例互斥量 `Local\TaskManagerLite.SingleInstance`：实例（含托盘常驻）运行时，安装、升级、卸载会提示先退出。该互斥量 DACL 只给 Everyone `SYNCHRONIZE`，足够 Inno 打开检测（已实测）。
- 安装完成后的“启动”以原用户令牌运行（`runasoriginaluser`），界面不会意外以管理员身份运行。
- winget：`scripts/winget-manifest.ps1` 生成多文件清单（`InstallerType: inno`、`Scope: machine`、`ProductCode` 为 `{AppId}_is1`）并 `winget validate`。是否、何时提交到 `microsoft/winget-pkgs` 由维护者决定，不在发布流程里自动提交。

## 方案比较

| 方案 | 结论 |
|---|---|
| Inno Setup | 采用。单文件、脚本简单，CI 镜像可用（缺失时 choco 安装），winget 原生支持其静默参数。 |
| MSI（WiX） | 否决。企业部署更友好，但自定义动作（登记 / 撤销 IFEO、询问覆盖）写起来繁琐，当前用户群用不上。 |
| 应用内“一键复制到 Program Files 再登记” | 否决。会出现两份 exe，版本同步、开机启动路径、卸载都要额外处理，等于自己实现半个安装器。 |
| 放宽位置检查 | 否决。见背景。 |

## 测试

- `pwsh scripts/test-installer.ps1`：编译测试构建（`-TestId`，互斥量名、AppId、快捷方式名带后缀），`/CURRENTUSER` 静默安装到临时目录，借 `TML_IFEO_TEST_KEY` / `TML_AUTOSTART_TEST_KEY` 只读写 HKCU 测试键。覆盖实例运行时拒绝安装、占用时不覆盖、登记写入、覆盖安装保留登记、卸载撤销登记与开机启动、删除文件与快捷方式。
- 真实的按机器安装（UAC、HKLM 登记、Ctrl+Shift+Esc 重定向）与向导界面列为人工验收。

## 已知限制

- 卸载程序以提权身份运行时，HKCU 指的是提权账户；用另一个管理员账户“越肩”提权卸载时，登录用户的开机启动登记不会被删除（指向的 exe 已不存在，登录时静默失败）。
- 测试构建与正式构建共用 `{autopf}` 默认目录，测试脚本始终用 `/DIR` 指定临时目录。
- exe 没有图标资源，开始菜单快捷方式和“应用和功能”里显示通用图标。
