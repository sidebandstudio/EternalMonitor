; EternalMonitor host installer (Inno Setup 6)
; Builds a single EternalMonitor-Setup.exe that installs the host app, the FFmpeg
; runtime, and (optionally) bundles + installs the Virtual Display Driver so the iPad
; can act as an extended display with no manual driver steps.
;
; Do not compile this directly — run scripts\build-installer.ps1, which stages the
; files and passes the required /D defines below:
;   StagingDir  : absolute path to the staged payload (app exe, DLLs, docs, driver\)
;   AppVersion  : version string, e.g. 0.1.1
;   IncludeDriver (optional, defined only when a driver setup .exe was staged)

#ifndef StagingDir
  #error "StagingDir is not defined — run scripts\build-installer.ps1 instead of compiling the .iss directly."
#endif
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
; Native Windows Ink pen injection requires the Windows 10 1809 APIs.
MinVersion=10.0.17763
AppId={{B7E6F2C4-3A91-4E0D-9C2A-ETERNALMONITOR}}
AppName=EternalMonitor
AppVersion={#AppVersion}
AppVerName=EternalMonitor {#AppVersion}
AppPublisher=Ali Younes
AppPublisherURL=https://github.com/whoisaldo/EternalMonitor
AppSupportURL=https://github.com/whoisaldo/EternalMonitor
AppUpdatesURL=https://github.com/whoisaldo/EternalMonitor/releases
AppContact=aliyounes@eternalreverse.com
DefaultDirName={autopf}\EternalMonitor
DefaultGroupName=EternalMonitor
DisableProgramGroupPage=yes
DisableDirPage=yes
; Driver installation requires elevation — request it once for the whole run.
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#StagingDir}\..\out
OutputBaseFilename=EternalMonitor-Setup
Compression=lzma2/max
SolidCompression=yes
; --- Branding (see DESIGN.md) -------------------------------------------------
WizardStyle=modern
SetupIconFile=..\host\assets\icon.ico
WizardImageFile=assets\wizard-large.bmp
WizardSmallImageFile=assets\wizard-small.bmp
WizardImageStretch=yes
UninstallDisplayName=EternalMonitor
UninstallDisplayIcon={app}\EternalMonitor-host.exe
SetupLogging=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"
; Autostart is handled inside the app (Settings -> "Start with Windows",
; which writes HKCU\Run for the signed-in user) — no installer task needed.

[Files]
Source: "{#StagingDir}\EternalMonitor-host.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StagingDir}\*.dll";                   DestDir: "{app}"; Flags: ignoreversion
Source: "{#StagingDir}\ffmpeg.exe";              DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#StagingDir}\README.md";               DestDir: "{app}"; Flags: ignoreversion isreadme skipifsourcedoesntexist
Source: "{#StagingDir}\LICENSE";                 DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#StagingDir}\QUICKSTART.txt";          DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
#ifdef IncludeDriver
; Third-party Virtual Display Driver setup, bundled so the tester never touches GitHub.
Source: "{#StagingDir}\driver\*"; DestDir: "{app}\driver"; Flags: ignoreversion recursesubdirs
; Scripts that register/remove the scheduled tasks the host uses to toggle the display, plus the
; toggle script the tasks invoke (it resolves the VDD device at trigger time).
Source: "scripts\vdd-tasks-setup.ps1";  DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "scripts\vdd-tasks-remove.ps1"; DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "scripts\vdd-driver-remove.ps1"; DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "scripts\vdd-driver-verify.ps1"; DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "scripts\vdd-toggle.ps1";       DestDir: "{app}\scripts"; Flags: ignoreversion
#endif

[Icons]
Name: "{group}\EternalMonitor";            Filename: "{app}\EternalMonitor-host.exe"
Name: "{group}\Quickstart (read me)";      Filename: "{app}\QUICKSTART.txt"
Name: "{group}\Uninstall EternalMonitor";  Filename: "{uninstallexe}"
Name: "{autodesktop}\EternalMonitor";      Filename: "{app}\EternalMonitor-host.exe"; Tasks: desktopicon

[Run]
; Replace only this product's named rules so upgrades remain idempotent.
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""EternalMonitor Host UDP"""; Flags: runhidden waituntilterminated
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""EternalMonitor Host TCP"""; Flags: runhidden waituntilterminated
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""EternalMonitor Host UDP"" dir=in action=allow program=""{app}\EternalMonitor-host.exe"" protocol=UDP profile=private,public enable=yes"; StatusMsg: "Allowing display traffic through Windows Firewall..."; Flags: runhidden waituntilterminated
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""EternalMonitor Host TCP"" dir=in action=allow program=""{app}\EternalMonitor-host.exe"" protocol=TCP profile=private,public enable=yes"; Flags: runhidden waituntilterminated
#ifdef IncludeDriver
; The vendor setup prompts to uninstall an existing installation even with silent
; switches. Preserve an existing driver and install only when it is absent.
Filename: "{app}\driver\vdd-setup-x64.exe"; Parameters: "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART"; StatusMsg: "Installing the virtual display driver (this enables the extended screen)..."; Flags: waituntilterminated; Check: NeedsVddInstall; AfterInstall: RecordVddOwnership
; Register the enable/disable scheduled tasks and leave the virtual display OFF by default —
; EternalMonitor turns it on only while streaming to it, so there's no phantom monitor.
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\vdd-tasks-setup.ps1"""; StatusMsg: "Configuring the on-demand virtual display..."; Flags: runhidden waituntilterminated; Check: VddInstallSucceeded; AfterInstall: VerifyVddDriver
#endif
; Launch the app at the end.
Filename: "{app}\EternalMonitor-host.exe"; Description: "Launch EternalMonitor now"; Flags: nowait postinstall skipifsilent; Check: VddInstallSucceeded

[UninstallRun]
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""EternalMonitor Host UDP"""; Flags: runhidden waituntilterminated; RunOnceId: "EternalMonitorFirewallUDP"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""EternalMonitor Host TCP"""; Flags: runhidden waituntilterminated; RunOnceId: "EternalMonitorFirewallTCP"
#ifdef IncludeDriver
; Remove the scheduled tasks and disable the device before removing an owned driver.
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\vdd-tasks-remove.ps1"""; Flags: runhidden; RunOnceId: "VddTasksRemove"
; Always record this entry to supersede older VddUninstall commands on upgrade.
; A Check that returns false at install time leaves the old command in place.
; The helper checks ownership when the uninstaller actually runs.
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\vdd-driver-remove.ps1"""; Flags: runhidden waituntilterminated; RunOnceId: "VddUninstall"
#endif

#ifdef IncludeDriver
[UninstallDelete]
Type: files; Name: "{app}\driver\installed-by-eternalmonitor.txt"
#endif

[Code]
var
  VddInstallFailed: Boolean;

function VddInstallSucceeded: Boolean;
begin
  Result := not VddInstallFailed;
end;

function GetCustomSetupExitCode: Integer;
begin
  Result := 0;
  if VddInstallFailed then
    Result := 1;
end;

#ifdef IncludeDriver
const
  VddRegistryKey = 'SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\VirtualDisplayDriver_is1';

function ReadVddRegistration(var Directory, Version: String): Boolean;
begin
  Result := RegQueryStringValue(HKLM64, VddRegistryKey, 'InstallLocation', Directory);
  if Result then
    Result := RegQueryStringValue(HKLM64, VddRegistryKey, 'DisplayVersion', Version)
  else begin
    Result := RegQueryStringValue(HKLM32, VddRegistryKey, 'InstallLocation', Directory);
    if Result then
      Result := RegQueryStringValue(HKLM32, VddRegistryKey, 'DisplayVersion', Version);
  end;
  Result := Result and (Directory <> '') and (Version <> '');
end;

function NeedsVddInstall: Boolean;
var
  Directory, Version: String;
begin
  Result := not ReadVddRegistration(Directory, Version);
  if not Result then
    Log('Preserving existing Virtual Display Driver package ' + Version);
end;

procedure VerifyVddDriver;
var
  ExitCode: Integer;
begin
  { [Run] ignores a child process's exit code. Check the actual PnP binding
    explicitly, including upgrades that preserve an existing registration. }
  if not Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
      '-NoProfile -ExecutionPolicy Bypass -File "' + ExpandConstant('{app}\scripts\vdd-driver-verify.ps1') + '"',
      '', SW_HIDE, ewWaitUntilTerminated, ExitCode) or (ExitCode <> 0) then begin
    VddInstallFailed := True;
    RaiseException('Windows did not finish installing the virtual display driver. Run setup while signed into Windows and approve its publisher prompt. If the driver already exists, repair it before trying again.');
  end;
end;

procedure RecordVddOwnership;
var
  Directory, Version: String;
begin
  VerifyVddDriver;
  if not ReadVddRegistration(Directory, Version) then
    RaiseException('Virtual Display Driver installation did not register successfully.');
  if not FileExists(AddBackslash(Directory) + 'unins000.exe') then
    RaiseException('Virtual Display Driver uninstaller is missing after installation.');
  if not SaveStringToFile(ExpandConstant('{app}\driver\installed-by-eternalmonitor.txt'),
      AddBackslash(Directory) + 'unins000.exe' + #13#10 + Version, False) then
    RaiseException('Could not record Virtual Display Driver installation ownership.');
end;

#endif
