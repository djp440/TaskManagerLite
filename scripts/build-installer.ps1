# 编译安装包：installer/TaskManagerLite.iss → target/installer/TaskManagerLite-<版本>-setup.exe。
# 版本取 tml-app 的 Cargo 版本；-Version 可覆盖（发布时传 tag 去掉 v 的部分，保留预发布后缀）。
# 需要 Inno Setup 6（winget install JRSoftware.InnoSetup；CI 镜像已自带，缺失时用 choco 安装）。
# 用法：pwsh scripts/build-installer.ps1 [-Version 0.3.0] [-Exe target/release/TaskManagerLite.exe] [-Build] [-TestId <id>]
param(
    [string]$Version,
    [string]$Exe = "$PSScriptRoot/../target/release/TaskManagerLite.exe",
    # 先执行 release 构建
    [switch]$Build,
    # 测试构建：互斥量名追加该后缀（与 TML_INSTANCE_ID 一致），AppId、显示名另起，供 test-installer.ps1 使用
    [string]$TestId
)

$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot/.."

if ($Build) {
    cargo build --locked -p tml-app --release
    if ($LASTEXITCODE) { throw "release 构建失败" }
}
if (-not $Version) {
    $Version = (cargo metadata --no-deps --format-version 1 --manifest-path "$root/Cargo.toml" | ConvertFrom-Json).packages |
        Where-Object name -eq 'tml-app' | Select-Object -ExpandProperty version
}
$exePath = (Resolve-Path $Exe).Path

$iscc = @(
    (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $iscc) { throw "找不到 Inno Setup 6 的 ISCC.exe，请先 winget install JRSoftware.InnoSetup" }

$defines = @("/DAppVersion=$Version", "/DSourceExe=$exePath")
if ($TestId) { $defines += "/DTestId=$TestId" }
& $iscc /Qp @defines "$root/installer/TaskManagerLite.iss"
if ($LASTEXITCODE) { throw "ISCC 编译失败（退出码 $LASTEXITCODE）" }
$out = Get-Item "$root/target/installer/TaskManagerLite-$Version-setup$(if ($TestId) { '-test' }).exe"
"{0}（{1:N2} MB）" -f $out.FullName, ($out.Length / 1MB)
