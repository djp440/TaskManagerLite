# 生成 winget 清单（多文件格式）到 target/winget/<版本>/，并用 winget validate 校验。只生成，不提交到 microsoft/winget-pkgs。
# 安装包的 SHA256 默认从 GitHub Releases 下载对应版本计算；-Installer 指定本地文件时直接用它（须与上传到 Release 的是同一个文件）。
# 用法：pwsh scripts/winget-manifest.ps1 [-Version 0.3.0] [-Installer target/installer/TaskManagerLite-0.3.0-setup.exe]
param(
    [string]$Version,
    [string]$Installer
)

$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot/.."
if (-not $Version) {
    $Version = (cargo metadata --no-deps --format-version 1 --manifest-path "$root/Cargo.toml" | ConvertFrom-Json).packages |
        Where-Object name -eq 'tml-app' | Select-Object -ExpandProperty version
}
$id = 'djp440.TaskManagerLite'
$schema = '1.10.0'
$repo = 'https://github.com/djp440/TaskManagerLite'
$url = "$repo/releases/download/v$Version/TaskManagerLite-$Version-setup.exe"

if ($Installer) {
    $hash = (Get-FileHash $Installer -Algorithm SHA256).Hash
} else {
    $download = Join-Path ([System.IO.Path]::GetTempPath()) "tml-winget-$Version-setup.exe"
    Invoke-WebRequest $url -OutFile $download
    $hash = (Get-FileHash $download -Algorithm SHA256).Hash
    Remove-Item $download
}

$out = Join-Path $root "target/winget/$Version"
New-Item -ItemType Directory -Force $out | Out-Null
Get-ChildItem $out -Filter '*.yaml' | Remove-Item

function Write-Manifest($name, $type, $body) {
    $text = "# yaml-language-server: `$schema=https://aka.ms/winget-manifest.$type.$schema.schema.json`n`n" +
        "PackageIdentifier: $id`nPackageVersion: $Version`n$body`nManifestType: $type`nManifestVersion: $schema`n"
    [System.IO.File]::WriteAllText((Join-Path $out $name), $text, [System.Text.UTF8Encoding]::new($false))
}

Write-Manifest "$id.yaml" 'version' 'DefaultLocale: en-US'

# 安装包按机器安装（Program Files）并自行请求提权；ProductCode 是 Inno 的卸载登记名（AppId + _is1）。
Write-Manifest "$id.installer.yaml" 'installer' @"
InstallerType: inno
Scope: machine
ElevationRequirement: elevatesSelf
UpgradeBehavior: install
ProductCode: '{F7DFE2D2-EB9C-41F1-8A19-DEE27FD47DC5}_is1'
ReleaseDate: $(Get-Date -Format 'yyyy-MM-dd')
Installers:
- Architecture: x64
  InstallerUrl: $url
  InstallerSha256: $hash
"@

Write-Manifest "$id.locale.en-US.yaml" 'defaultLocale' @"
PackageLocale: en-US
Publisher: djp440
PublisherUrl: https://github.com/djp440
PublisherSupportUrl: $repo/issues
PackageName: TaskManagerLite
PackageUrl: $repo
License: MIT
LicenseUrl: $repo/blob/main/LICENSE
ShortDescription: A Windows Task Manager rewritten in Rust with Slint - fast cold start, low idle overhead, and a polished UI.
Tags:
- task-manager
- process
- performance
- system-monitor
ReleaseNotesUrl: $repo/releases/tag/v$Version
"@

Write-Manifest "$id.locale.zh-CN.yaml" 'locale' @"
PackageLocale: zh-CN
Publisher: djp440
PackageName: TaskManagerLite
License: MIT
ShortDescription: 用 Rust + Slint 重写的 Windows 任务管理器：冷启动快、常驻开销低、界面美观。
Tags:
- 任务管理器
- 进程
- 性能
"@

winget validate --manifest $out
if ($LASTEXITCODE) { throw "winget validate 失败（退出码 $LASTEXITCODE）" }
$out
