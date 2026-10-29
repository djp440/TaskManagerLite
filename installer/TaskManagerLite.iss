; TaskManagerLite 安装包（Inno Setup 6）。用 scripts/build-installer.ps1 编译，不要直接调用 ISCC。
; 默认按机器安装到 Program Files：只有管理员可写，满足接管任务管理器的位置检查（ADR 0003、0005）。
; 命令行加 /CURRENTUSER 可改为按用户安装（%LOCALAPPDATA%\Programs），自动化测试用它免提权。

#ifndef AppVersion
  #error 需要 /DAppVersion=<版本号>
#endif
#ifndef SourceExe
  #error 需要 /DSourceExe=<TaskManagerLite.exe 路径>
#endif

#define AppName "TaskManagerLite"
#define ExeName "TaskManagerLite.exe"
; 测试构建（scripts/test-installer.ps1）：互斥量名追加 TML_INSTANCE_ID 后缀，AppId、显示名与输出文件名另起，
; 不会被真实实例挡住，也不会覆盖真实安装的卸载登记与快捷方式。
#ifdef TestId
  #define AppIdGuid "{F7DFE2D2-EB9C-41F1-8A19-DEE27FD47DC5}-test"
  #define DisplayName AppName + " (test " + TestId + ")"
  #define MutexName "Local\TaskManagerLite.SingleInstance." + TestId
  #define OutputName AppName + "-" + AppVersion + "-setup-test"
#else
  #define AppIdGuid "{F7DFE2D2-EB9C-41F1-8A19-DEE27FD47DC5}"
  #define DisplayName AppName
  #define MutexName "Local\TaskManagerLite.SingleInstance"
  #define OutputName AppName + "-" + AppVersion + "-setup"
#endif
; VersionInfoVersion 只接受数字，预发布后缀（如 0.3.0-beta.1 的 -beta.1）去掉。
#define DashPos Pos("-", AppVersion)
#if DashPos > 0
  #define NumericVersion Copy(AppVersion, 1, DashPos - 1)
#else
  #define NumericVersion AppVersion
#endif

[Setup]
; AppId 决定升级与卸载登记（注册表 ..\Uninstall\{AppId}_is1，winget 清单的 ProductCode），发布后不能再改。
AppId={{#AppIdGuid}
AppName={#DisplayName}
AppVersion={#AppVersion}
AppVerName={#DisplayName} {#AppVersion}
AppPublisher=djp440
AppPublisherURL=https://github.com/djp440/TaskManagerLite
AppSupportURL=https://github.com/djp440/TaskManagerLite/issues
AppUpdatesURL=https://github.com/djp440/TaskManagerLite/releases
VersionInfoVersion={#NumericVersion}
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=admin
PrivilegesRequiredOverridesAllowed=commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; 主实例持有的单实例互斥量：安装、升级、卸载前提示用户先退出正在运行的实例（含托盘常驻）。
AppMutex={#MutexName}
UninstallDisplayIcon={app}\{#ExeName}
UninstallDisplayName={#DisplayName}
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
OutputDir=..\target\installer
OutputBaseFilename={#OutputName}

[Languages]
Name: "chs"; MessagesFile: "ChineseSimplified.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
chs.ReplaceTaskmgr=替换系统任务管理器（Ctrl+Shift+Esc 打开本程序，可随时在设置页关闭）
en.ReplaceTaskmgr=Replace Windows Task Manager (Ctrl+Shift+Esc opens this app; can be turned off in Settings)
chs.TaskmgrOccupied=系统任务管理器已被其他程序接管：%n%1%n%n是否改为由 TaskManagerLite 接管？卸载 TaskManagerLite 时不会还原原来的程序。
en.TaskmgrOccupied=Task Manager is already replaced by another program:%n%1%n%nReplace it with TaskManagerLite instead? Uninstalling TaskManagerLite will not restore the previous program.
chs.TaskmgrFailed=未能替换系统任务管理器（退出码 %1）。安装已完成，可以稍后在设置页重试。
en.TaskmgrFailed=Could not replace Task Manager (exit code %1). Installation finished; you can retry later in Settings.
chs.LaunchApp=启动 TaskManagerLite
en.LaunchApp=Launch TaskManagerLite

[Tasks]
Name: "replacetaskmgr"; Description: "{cm:ReplaceTaskmgr}"; Flags: unchecked
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "{#ExeName}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#DisplayName}"; Filename: "{app}\{#ExeName}"
Name: "{autodesktop}\{#DisplayName}"; Filename: "{app}\{#ExeName}"; Tasks: desktopicon

[Run]
; 安装程序是提权的，启动应用时退回原用户令牌，免得界面进程意外以管理员身份运行。
Filename: "{app}\{#ExeName}"; Description: "{cm:LaunchApp}"; Flags: nowait postinstall skipifsilent runasoriginaluser

[Code]
const
  { 与 crates/core/src/taskmgr.rs 的退出码一致。 }
  ExitOccupied = 3;
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  ApprovedKey = 'Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run';
  TestKeyPrefix = 'software\taskmanagerlite-test\';

function AppExe: String;
begin
  Result := ExpandConstant('{app}\{#ExeName}');
end;

{ 运行本程序的命令行开关，返回退出码；进程没能启动时返回 -1。 }
function RunApp(Params: String): Integer;
begin
  if not Exec(AppExe, Params, '', SW_HIDE, ewWaitUntilTerminated, Result) then
    Result := -1;
  Log(Format('%s %s -> %d', [AppExe, Params, Result]));
end;

{ 安装程序已提权，--register-taskmgr 直接写 HKLM，不会再弹 UAC。被其他程序占用时征得同意才 --force。 }
procedure RegisterTaskmgr;
var
  Code: Integer;
  Current: String;
begin
  Code := RunApp('--register-taskmgr');
  if Code = ExitOccupied then
  begin
    if not RegQueryStringValue(HKLM64, 'SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\Taskmgr.exe', 'Debugger', Current) then
      Current := '?';
    if SuppressibleMsgBox(FmtMessage(CustomMessage('TaskmgrOccupied'), [Current]), mbConfirmation, MB_YESNO, IDNO) = IDYES then
      Code := RunApp('--register-taskmgr --force')
    else
      exit;
  end;
  if Code <> 0 then
    SuppressibleMsgBox(FmtMessage(CustomMessage('TaskmgrFailed'), [IntToStr(Code)]), mbError, MB_OK, IDOK);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('replacetaskmgr') then
    RegisterTaskmgr;
end;

{ 开机启动登记在 HKCU，只删指向本次安装位置的那一项；与应用一样认 TML_AUTOSTART_TEST_KEY 测试键。 }
procedure RemoveAutostart;
var
  Base, Run, Approved, Value: String;
begin
  Base := GetEnv('TML_AUTOSTART_TEST_KEY');
  if Base <> '' then
  begin
    if Pos(TestKeyPrefix, Lowercase(Base)) <> 1 then
      exit;
    Run := Base + '\Run';
    Approved := Base + '\Approved';
  end else begin
    Run := RunKey;
    Approved := ApprovedKey;
  end;
  if RegQueryStringValue(HKCU, Run, '{#AppName}', Value) and (Pos(Lowercase(AppExe), Lowercase(Value)) > 0) then
  begin
    RegDeleteValue(HKCU, Run, '{#AppName}');
    RegDeleteValue(HKCU, Approved, '{#AppName}');
    Log('已删除开机启动登记：' + Value);
  end;
end;

{ 删除文件之前撤销接管：exe 不存在时 Ctrl+Shift+Esc 连系统任务管理器都打不开。
  --unregister-taskmgr 不带 --force，只删指向本程序或已失效的登记，不动其他程序的。 }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
  begin
    RunApp('--unregister-taskmgr');
    RemoveAutostart;
  end;
end;
