# 0003 用 IFEO `Debugger` 接管 Ctrl+Shift+Esc

状态：待审阅（2026-10-28，对应 issue #7）。实现随 ADR 一起提交，但设置页入口依赖 #16，默认不启用。

## 背景

用户希望 Ctrl+Shift+Esc（以及 Win+X 菜单、任务栏右键等所有启动 `Taskmgr.exe` 的入口）打开本程序，而不是系统自带的任务管理器。这些入口最终都是启动 `%SystemRoot%\System32\Taskmgr.exe`，没有可以直接注册的“默认任务管理器”设置，也没有公开的快捷键注册接口能覆盖系统保留的 Ctrl+Shift+Esc。

## 方案比较

| 方案 | 结论 |
|---|---|
| 映像文件执行选项（IFEO）`Debugger` | 采用。Process Explorer 的“Replace Task Manager”用的就是它，行为经过多年验证；覆盖所有启动 `Taskmgr.exe` 的入口；不改系统文件；撤销只需删一个值。 |
| 替换或改名 `System32\Taskmgr.exe` | 否决。受 TrustedInstaller 保护，要改系统文件和 ACL，系统更新、SFC 会还原或报错，风险远高于收益。 |
| 注册全局热键 / 键盘钩子 | 否决。Ctrl+Shift+Esc 由系统在更底层处理，不能可靠拦截；常驻钩子增加开销，也会被杀毒软件当成键盘记录。 |
| 应用兼容层 `AppCompat` 或 `AppPaths`、文件关联 | 否决。不会影响 `Taskmgr.exe` 的启动，或只对 Shell 的部分入口生效。 |

## 决定

### 登记位置与格式

- 键：`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\Taskmgr.exe`（始终访问 64 位视图）。
- 值：`Debugger`，`REG_SZ`，内容为**带引号的 exe 完整路径**，例如 `"C:\Tools\TaskManagerLite.exe"`。路径含空格时不加引号会被解析成多个词；不附加任何参数。
- 系统启动 `Taskmgr.exe` 时把原命令行接在 `Debugger` 值之后启动调试器，所以本程序被启动时的命令行是 `"<本程序>" "C:\WINDOWS\system32\taskmgr.exe" [页签参数]`。第一个参数是 `taskmgr.exe` 的路径，之后可能有 `/0` 之类的页签参数，一律忽略（`launched_as_taskmgr_debugger` 只用于写日志）。本程序的命令行解析只认 `--` 开头的开关，位置参数不会被误解。
- 不展开环境变量：值里含 `%…%` 时按“其他程序”处理。

### 启用 / 还原流程

- 启用：先只读检查现状。
  - 已指向本 exe：什么都不做。
  - 指向**别的有效程序**（文件存在，例如已装 Process Explorer）：拒绝覆盖，提示用户；用户明确确认后才可 `--force`。
  - 未登记，或指向的文件已不存在：写入 `Debugger`。
  - 同时要求本 exe 文件存在。
- 撤销：删除 `Debugger` 值，键里没有其他值时顺带删除空键，恢复到登记前的样子。规则与启用对称：本 exe 的登记或失效的登记可直接删；他人的有效登记要 `--force`；本来没登记则空操作。
- “意愿”与“事实”分开：设置文件的 `replace_taskmgr` 只记录用户想不想接管；是否真正生效以注册表为准，设置页应显示注册表的实际状态。

### 提权

写 HKLM 需要管理员权限，而本程序平时以普通权限运行（提权是用户主动的“以管理员身份重启”）。启用或撤销时：

1. 先直接尝试写；成功则结束（已提权的实例就是这种情况）。
2. 得到“拒绝访问”且当前未提权时，用 `ShellExecuteEx` 的 `runas` 启动**一次性的提权子进程**：本 exe 加 `--register-taskmgr` 或 `--unregister-taskmgr`（可带 `--force`），隐藏窗口，等待它退出，用退出码判断结果（见下）。用户在 UAC 中取消得到 `Cancelled`。
3. 子进程在 `main` 的最开头处理这些开关：只做注册表读写，不创建窗口、不占单实例锁，随后立即退出。

不复用“以管理员身份重启”：那会让整个界面进程提权并关闭、重开主窗口，仅仅为了写一个值，代价过大，也会打断当前会话。子进程写入的路径是它自己的 `current_exe()`，不接受外部传入的路径，避免被拿来写任意值。

### exe 被移动或删除之后

这是本方案最主要的风险，必须说明白：

- 系统启动 `Taskmgr.exe` 时，`Debugger` 指向的文件不存在，进程创建会失败（系统弹出找不到文件的错误），**连系统自带任务管理器也打不开了**，Ctrl+Shift+Esc 看起来“失灵”。Process Explorer 有同样的行为。这一点按系统文档的行为推断，没有在本机上故意制造该状态验证。
- 检测：本程序每次启动，在“接管”意愿为开时读取该值并写入日志（路径是否指向自己、是否已失效）；设置页（#16）应据此提示“登记的路径已失效/不是当前位置”，并提供“重新登记到当前位置”（走上面的提权流程）。检测只读一个值，对冷启动没有可测量的影响。
- 失效的登记不算被占用：从新位置启用时可以直接覆盖，不需要 `--force`。
- 如果 exe 已被删除而本程序根本跑不起来，就无法自检。此时必须手动还原（见下）。因此：
  - 卸载或删除前，应先在设置页关闭接管；
  - 文档和设置页的说明里要写出手动还原命令；
  - 建议把 exe 放在固定位置（如 `C:\Program Files\TaskManagerLite`），不放在会清理的下载目录。
- 不做“后台守护进程”或计划任务来自动修复：会增加常驻开销，也扩大了写 HKLM 的攻击面。

### 撤销方式

任选其一（后两种在本程序无法运行时使用）：

1. 设置页关闭开关（或 `TaskManagerLite.exe --unregister-taskmgr`）。
2. 管理员命令提示符：`reg delete "HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\Taskmgr.exe" /v Debugger /f`
3. 注册表编辑器中删除该键下的 `Debugger` 值（`Taskmgr.exe` 键本身在干净的系统上通常不存在，也可整个删除）。

即使本机的任务管理器已因路径失效打不开，也可以用 Win+R 运行 `regedit`，或在开始菜单的“运行”里输入 `cmd` 提权后执行上面的命令。

### 命令行开关与退出码

- `--register-taskmgr [--force]`：登记。`--unregister-taskmgr [--force]`：撤销。未提权且需要写 HKLM 时自动弹出 UAC 并由提权子进程完成。
- `--taskmgr-status`：只读查询，退出码即结果。
- 退出码：`0` 成功（状态查询时表示已指向本 exe）；`1` 其他失败；`2` 拒绝访问；`3` 被其他程序占用；`4` 用户取消 UAC；状态查询另有 `10` 未登记、`11` 指向其他有效程序、`12` 指向已失效的路径。
- 设置页应在**后台线程**调用 `tml_core::set_replace_taskmgr(enable, force)`（会阻塞到 UAC 与子进程结束），成功后再写入设置的 `replace_taskmgr`。

### 测试隔离

- 注册表根键与路径可注入（`Ifeo::under`）；集成测试只读写 `HKCU\Software\TaskManagerLite-test\<随机名>`，结束时（含 panic）删除。
- 环境变量 `TML_IFEO_TEST_KEY` 可让上述命令行开关改写 `HKCU` 下的测试键（值必须以 `Software\TaskManagerLite-test\` 开头，否则报错而不是退回真实键），用于端到端脚本 `scripts/test-taskmgr-ifeo.ps1`，全程不提权、不碰 HKLM。
- 真实的 HKLM 写入、UAC 提示和 Ctrl+Shift+Esc 重定向无法安全自动化，列为人工验收。

## 取舍与已知限制

- 把可写路径当作代码执行入口：`Debugger` 指向的 exe 会在每次有人打开任务管理器时运行。若 exe 放在普通用户可写的目录，恶意软件替换它就能在用户（包括管理员用户）下一次按 Ctrl+Shift+Esc 时获得代码执行机会。建议放在仅管理员可写的位置（如 Program Files）；设置页可在 exe 位于用户可写目录时给出提醒。本版本不强制校验。
- 只影响启动 `Taskmgr.exe` 进程的入口（Ctrl+Shift+Esc、任务栏右键、Win+X、运行框等）。Ctrl+Alt+Del 安全屏幕上的“任务管理器”也是启动 `Taskmgr.exe`，预期同样被重定向，但它与普通桌面的交互行为没有在本机验证，列入人工验收观察项。
- 系统自带任务管理器只有在登记期间不可用；撤销后立即恢复，无需重启。
- 单实例（#12）已有：登记后重复按 Ctrl+Shift+Esc 会把已打开的窗口带到前台，而不是多开。
