# “应用”判定的真实桌面验收：启动记事本、无窗口子进程、计算器（UWP，本机有则测），
# 断言判定结果，关闭窗口后断言不再是应用，并测量窗口枚举的单次耗时。
# 只启动、只结束脚本自己创建的进程。需要交互式桌面。
# 用法：pwsh scripts/test-apps.ps1
$ErrorActionPreference = 'Stop'
Set-Location "$PSScriptRoot/.."
cargo test -p tml-core --test apps -- --ignored --nocapture --test-threads=1
exit $LASTEXITCODE
