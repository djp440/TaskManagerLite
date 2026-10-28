# “应用”判定的真实桌面验收：启动记事本、无窗口子进程、计算器（UWP，本机有则测），
# 断言判定结果，关闭窗口后断言不再是应用，并测量窗口枚举的单次耗时。
# 另有“其他虚拟桌面上的应用”场景：用 IVirtualDesktopManager::MoveWindowToDesktop 把测试自己创建的窗口
# （以及自己启动的记事本，系统若拒绝跨进程移动则只测前者）移到另一个已存在的虚拟桌面，断言仍是应用。
# 只用你已经建好的桌面，不会新建 / 删除 / 切换桌面；只有一个桌面时该项输出“跳过”并通过。
# 想真正跑到它：先在任务视图（Win+Tab）里新建一个桌面，跑完再自行删除。
# 只启动、只结束脚本自己创建的进程。需要交互式桌面。
# 用法：pwsh scripts/test-apps.ps1
$ErrorActionPreference = 'Stop'
Set-Location "$PSScriptRoot/.."
cargo test -p tml-core --test apps -- --ignored --nocapture --test-threads=1
exit $LASTEXITCODE
