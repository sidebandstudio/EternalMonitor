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
;   Sign        (optional, defined with the "eternal" sign tool, /Seternal=<command>)

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
AppPublisher=Sideband Studio
AppPublisherURL=https://github.com/sidebandstudio/EternalMonitor
AppSupportURL=https://github.com/sidebandstudio/EternalMonitor
AppUpdatesURL=https://github.com/sidebandstudio/EternalMonitor/releases
AppContact=hello@sideband.studio
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
; --- Branding (see docs/design.md) --------------------------------------------
WizardStyle=modern
SetupIconFile=..\host\assets\icon.ico
WizardImageFile=assets\wizard-large.bmp
WizardSmallImageFile=assets\wizard-small.bmp
WizardImageStretch=yes
UninstallDisplayName=EternalMonitor
UninstallDisplayIcon={app}\EternalMonitor-host.exe
SetupLogging=yes
; Ask for the language every time, with the Windows display language
; preselected (not the one an earlier install used).
ShowLanguageDialog=yes
LanguageDetectionMethod=uilanguage
UsePreviousLanguage=no
#ifdef Sign
; Authenticode-signs Setup, the uninstaller it writes and the [Files] entries
; flagged "sign" (see docs/windows-signing.md).
SignTool=eternal
#endif

[Languages]
; The app's eleven languages (i18n/glossary.md). Official translations come
; with Inno Setup; Chinese Simplified and Hindi are the unofficial ones from
; jrsoftware/issrc, kept in languages\ (see its README.txt).
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "arabic"; MessagesFile: "compiler:Languages\Arabic.isl"
Name: "chinesesimplified"; MessagesFile: "languages\ChineseSimplified.isl"
Name: "hindi"; MessagesFile: "languages\Hindi.islu"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"
Name: "brazilianportuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"
Name: "french"; MessagesFile: "compiler:Languages\French.isl"
Name: "german"; MessagesFile: "compiler:Languages\German.isl"
Name: "korean"; MessagesFile: "compiler:Languages\Korean.isl"

[Messages]
; The same requirements the website, README and SETUP.md list.
WinVersionTooLowError=EternalMonitor needs Windows 10 version 1809 or later, or Windows 11. Update Windows, then run this installer again.
OnlyOnTheseArchitectures=EternalMonitor needs a 64-bit PC with Windows 10 version 1809 or later, or Windows 11.

[CustomMessages]
; AppLanguageCode is the app's code for each language (see i18n::Lang).
english.AppLanguageCode=en
english.DesktopIconTask=Create a desktop shortcut
english.ShortcutsGroup=Shortcuts:
english.LaunchNow=Launch EternalMonitor now
english.FirewallStatus=Allowing display traffic through Windows Firewall...
english.DriverStatus=Installing the virtual display driver (this enables the extended screen)...
english.DisplayStatus=Configuring the on-demand virtual display...
spanish.AppLanguageCode=es
spanish.DesktopIconTask=Crear un acceso directo en el escritorio
spanish.ShortcutsGroup=Accesos directos:
spanish.LaunchNow=Abrir EternalMonitor ahora
spanish.FirewallStatus=Permitiendo el tráfico de pantalla en el Firewall de Windows...
spanish.DriverStatus=Instalando el controlador de pantalla virtual (activa la segunda pantalla)...
spanish.DisplayStatus=Configurando la pantalla virtual bajo demanda...
arabic.AppLanguageCode=ar
arabic.DesktopIconTask=إنشاء اختصار على سطح المكتب
arabic.ShortcutsGroup=الاختصارات:
arabic.LaunchNow=تشغيل EternalMonitor الآن
arabic.FirewallStatus=جارٍ السماح بحركة بيانات العرض عبر جدار حماية Windows...
arabic.DriverStatus=جارٍ تثبيت برنامج تشغيل الشاشة الافتراضية (يتيح الشاشة الثانية)...
arabic.DisplayStatus=جارٍ إعداد الشاشة الافتراضية عند الطلب...
chinesesimplified.AppLanguageCode=zh-Hans
chinesesimplified.DesktopIconTask=创建桌面快捷方式
chinesesimplified.ShortcutsGroup=快捷方式：
chinesesimplified.LaunchNow=立即运行 EternalMonitor
chinesesimplified.FirewallStatus=正在允许显示流量通过 Windows 防火墙...
chinesesimplified.DriverStatus=正在安装虚拟显示器驱动程序（用于第二屏幕）...
chinesesimplified.DisplayStatus=正在配置按需启用的虚拟显示器...
hindi.AppLanguageCode=hi
hindi.DesktopIconTask=डेस्कटॉप शॉर्टकट बनाएँ
hindi.ShortcutsGroup=शॉर्टकट:
hindi.LaunchNow=EternalMonitor अभी खोलें
hindi.FirewallStatus=Windows फ़ायरवॉल में डिस्प्ले ट्रैफ़िक की अनुमति दी जा रही है...
hindi.DriverStatus=वर्चुअल डिस्प्ले ड्राइवर इंस्टॉल हो रहा है (इससे दूसरी स्क्रीन चलती है)...
hindi.DisplayStatus=ज़रूरत पड़ने पर चलने वाला वर्चुअल डिस्प्ले सेट हो रहा है...
russian.AppLanguageCode=ru
russian.DesktopIconTask=Создать ярлык на рабочем столе
russian.ShortcutsGroup=Ярлыки:
russian.LaunchNow=Запустить EternalMonitor
russian.FirewallStatus=Разрешение трафика экрана в брандмауэре Windows...
russian.DriverStatus=Установка драйвера виртуального дисплея (он включает второй экран)...
russian.DisplayStatus=Настройка виртуального дисплея по запросу...
japanese.AppLanguageCode=ja
japanese.DesktopIconTask=デスクトップにショートカットを作成
japanese.ShortcutsGroup=ショートカット:
japanese.LaunchNow=EternalMonitor を今すぐ起動
japanese.FirewallStatus=Windows ファイアウォールで画面の通信を許可しています...
japanese.DriverStatus=仮想ディスプレイ ドライバーをインストールしています (セカンドスクリーンに必要です)...
japanese.DisplayStatus=必要なときだけ使う仮想ディスプレイを設定しています...
brazilianportuguese.AppLanguageCode=pt-BR
brazilianportuguese.DesktopIconTask=Criar um atalho na área de trabalho
brazilianportuguese.ShortcutsGroup=Atalhos:
brazilianportuguese.LaunchNow=Abrir o EternalMonitor agora
brazilianportuguese.FirewallStatus=Liberando o tráfego de tela no Firewall do Windows...
brazilianportuguese.DriverStatus=Instalando o driver de tela virtual (ele ativa a segunda tela)...
brazilianportuguese.DisplayStatus=Configurando a tela virtual sob demanda...
french.AppLanguageCode=fr
french.DesktopIconTask=Créer un raccourci sur le Bureau
french.ShortcutsGroup=Raccourcis :
french.LaunchNow=Lancer EternalMonitor maintenant
french.FirewallStatus=Autorisation du trafic d'affichage dans le Pare-feu Windows...
french.DriverStatus=Installation du pilote d'affichage virtuel (il active le second écran)...
french.DisplayStatus=Configuration de l'affichage virtuel à la demande...
german.AppLanguageCode=de
german.DesktopIconTask=Desktopverknüpfung erstellen
german.ShortcutsGroup=Verknüpfungen:
german.LaunchNow=EternalMonitor jetzt starten
german.FirewallStatus=Bildschirmübertragung wird in der Windows-Firewall zugelassen...
german.DriverStatus=Virtueller Anzeigetreiber wird installiert (er ermöglicht den zweiten Bildschirm)...
german.DisplayStatus=Virtuelle Anzeige auf Abruf wird eingerichtet...
korean.AppLanguageCode=ko
korean.DesktopIconTask=바탕 화면에 바로 가기 만들기
korean.ShortcutsGroup=바로 가기:
korean.LaunchNow=지금 EternalMonitor 실행
korean.FirewallStatus=Windows 방화벽에서 화면 트래픽을 허용하는 중...
korean.DriverStatus=가상 디스플레이 드라이버 설치 중(두 번째 화면에 필요)...
korean.DisplayStatus=필요할 때 켜지는 가상 디스플레이 구성 중...

[Tasks]
Name: "desktopicon"; Description: "{cm:DesktopIconTask}"; GroupDescription: "{cm:ShortcutsGroup}"
; Autostart is handled inside the app (Settings -> "Start with Windows",
; which writes HKCU\Run for the signed-in user) — no installer task needed.

[Files]
; "sign" covers our own two exes. Inno ignores it when SignTool isn't set.
; Third-party files ship exactly as their publishers built them.
Source: "{#StagingDir}\EternalMonitor-host.exe"; DestDir: "{app}"; Flags: ignoreversion sign
; In-app updates run a copy of this helper from the user's update folder.
Source: "{#StagingDir}\EternalMonitor-updater.exe"; DestDir: "{app}"; Flags: ignoreversion sign
Source: "{#StagingDir}\*.dll";                   DestDir: "{app}"; Flags: ignoreversion
Source: "{#StagingDir}\ffmpeg.exe";              DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#StagingDir}\README.md";               DestDir: "{app}"; Flags: ignoreversion isreadme skipifsourcedoesntexist
Source: "{#StagingDir}\LICENSE";                 DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#StagingDir}\QUICKSTART.txt";          DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#StagingDir}\Geist-OFL.txt";           DestDir: "{app}"; Flags: ignoreversion
Source: "{#StagingDir}\Noto-OFL.txt";            DestDir: "{app}"; Flags: ignoreversion
#ifdef IncludeDriver
; Third-party Virtual Display Driver setup, bundled so the tester never touches GitHub.
Source: "{#StagingDir}\driver\*"; DestDir: "{app}\driver"; Flags: ignoreversion recursesubdirs
; Scripts that register/remove the scheduled tasks the host uses to toggle the display, plus the
; toggle script the tasks invoke (it resolves the VDD device at trigger time).
Source: "scripts\vdd-tasks-setup.ps1";  DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "scripts\vdd-tasks-remove.ps1"; DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "scripts\vdd-driver-remove.ps1"; DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "scripts\vdd-driver-verify.ps1"; DestDir: "{app}\scripts"; Flags: ignoreversion
Source: "scripts\vdd-driver-repair.ps1"; DestDir: "{app}\scripts"; Flags: ignoreversion
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
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""EternalMonitor Host UDP"" dir=in action=allow program=""{app}\EternalMonitor-host.exe"" protocol=UDP profile=domain,private,public enable=yes"; StatusMsg: "{cm:FirewallStatus}"; Flags: runhidden waituntilterminated
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""EternalMonitor Host TCP"" dir=in action=allow program=""{app}\EternalMonitor-host.exe"" protocol=TCP profile=domain,private,public enable=yes"; Flags: runhidden waituntilterminated
#ifdef IncludeDriver
; The vendor setup prompts to uninstall an existing installation even with silent
; switches. Preserve an existing driver and install only when it is absent.
Filename: "{app}\driver\vdd-setup-x64.exe"; Parameters: "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART"; StatusMsg: "{cm:DriverStatus}"; Flags: waituntilterminated; Check: NeedsVddInstall; AfterInstall: RecordVddOwnership
; Register the enable/disable scheduled tasks and leave the virtual display OFF by default —
; EternalMonitor turns it on only while streaming to it, so there's no phantom monitor.
; A preserved driver whose device was removed or broke is repaired first.
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\vdd-tasks-setup.ps1"""; StatusMsg: "{cm:DisplayStatus}"; Flags: runhidden waituntilterminated; Check: VddInstallSucceeded; BeforeInstall: RepairVddDriver; AfterInstall: VerifyVddDriver
#endif
; Launch the app at the end.
Filename: "{app}\EternalMonitor-host.exe"; Description: "{cm:LaunchNow}"; Flags: nowait postinstall skipifsilent; Check: VddInstallSucceeded

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

[UninstallDelete]
Type: files; Name: "{app}\language.txt"
#ifdef IncludeDriver
Type: files; Name: "{app}\driver\installed-by-eternalmonitor.txt"
#endif

[Code]
var
  VddInstallFailed: Boolean;

function VddInstallSucceeded: Boolean;
begin
  Result := not VddInstallFailed;
end;

{ Inno Setup's own exit codes are 1 to 8 (1: Setup failed to initialize).
  The update helper reports Setup's code to the relaunched app, so this one
  stays clear of them. }
function GetCustomSetupExitCode: Integer;
begin
  Result := 0;
  if VddInstallFailed then
    Result := 20;
end;

const
  GpuPreferencesKey = 'Software\Microsoft\DirectX\UserGpuPreferences';

procedure CurStepChanged(CurStep: TSetupStep);
begin
  // The app shows the language picked here until one is picked in the app
  // (i18n::installer_language). Setup runs elevated, so it writes next to
  // the exe and never the user's settings. A silent run, such as an in-app
  // update, asked nothing and keeps the earlier choice.
  if (CurStep = ssPostInstall) and not WizardSilent then
    if not SaveStringToFile(ExpandConstant('{app}\language.txt'),
        ExpandConstant('{cm:AppLanguageCode}'), False) then
      Log('Could not save the language choice');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  // The app's own graphics card choice, which it writes only when its user
  // asks (Windows Settings > Display > Graphics writes the same value). The
  // key and every other app's value stay. HKCU is the user who uninstalls.
  if CurUninstallStep = usUninstall then
    RegDeleteValue(HKEY_CURRENT_USER, GpuPreferencesKey, ExpandConstant('{app}\EternalMonitor-host.exe'));
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

procedure RepairVddDriver;
var
  ExitCode: Integer;
  Message: String;
begin
  { Setup keeps an existing driver instead of running its setup again. When
    Setup installed it and no device works, put one back (exit codes in the
    script). A driver another app installed is left alone. }
  if not Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
      '-NoProfile -ExecutionPolicy Bypass -File "' + ExpandConstant('{app}\scripts\vdd-driver-repair.ps1') + '"',
      '', SW_HIDE, ewWaitUntilTerminated, ExitCode) then
    ExitCode := -1;
  if ExitCode = 0 then
    exit;
  case ExitCode of
    10: Message := 'Another app installed the virtual display driver, and none of its displays works. EternalMonitor does not change another app''s driver. Repair it in that app, or uninstall "Virtual Display Driver" in Windows Settings > Apps > Installed apps, then run this setup again.';
    11: Message := 'The virtual display driver''s files are missing. Uninstall "Virtual Display Driver" in Windows Settings > Apps > Installed apps, then run this setup again.';
  else
    Message := 'Setup could not repair the virtual display driver. Restart the PC and run this setup again. If it still fails, uninstall "Virtual Display Driver" in Windows Settings > Apps > Installed apps, then run this setup again.';
  end;
  VddInstallFailed := True;
  RaiseException(Message);
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
