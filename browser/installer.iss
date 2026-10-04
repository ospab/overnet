; Установщик overnet browser для Windows (Inno Setup 6).
;
; Ставит перепакованный браузер (папка Browser, рядом с ней профиль Data) без
; прав администратора, с ярлыками и удалением через «Приложения» Windows.
; Собирается в .github/workflows/release.yml:
;   ISCC /DAppVersion=0.2.5 /DSourceDir=dist\overnet-browser\Browser /DIconFile=dist\icons\overnet.ico browser\installer.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\dist\overnet-browser\Browser"
#endif
#ifndef IconFile
  #define IconFile "..\dist\icons\overnet.ico"
#endif

[Setup]
; AppId не менять: по нему Windows узнаёт установленную копию при обновлении.
AppId={{6B2E9C1A-4F7D-4C55-9A3E-0D8F1E2B7C40}
AppName=overnet browser
AppVersion={#AppVersion}
AppVerName=overnet browser {#AppVersion}
AppPublisher=overnet
AppPublisherURL=https://github.com/ospab/overnet
AppSupportURL=https://github.com/ospab/overnet/issues
AppUpdatesURL=https://github.com/ospab/overnet/releases
; Без прав администратора: всё в профиле пользователя.
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\overnet-browser
DisableDirPage=auto
DisableProgramGroupPage=yes
UninstallDisplayName=overnet browser
UninstallDisplayIcon={app}\Browser\overnet-browser.exe
SetupIconFile={#IconFile}
WizardStyle=modern
Compression=lzma2/ultra64
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; При обновлении закрыть запущенный браузер и его шлюз.
CloseApplications=force
RestartApplications=no
OutputBaseFilename=overnet-browser-setup
ShowLanguageDialog=auto

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"

[CustomMessages]
en.DesktopIcon=Create a &desktop shortcut
ru.DesktopIcon=Создать ярлык на &рабочем столе
en.LaunchNow=Open overnet browser
ru.LaunchNow=Открыть overnet browser

[Tasks]
Name: "desktopicon"; Description: "{cm:DesktopIcon}"; Flags: unchecked

[InstallDelete]
; Программа заменяется целиком (старые файлы новой версии могут помешать);
; профиль в Data остаётся.
Type: filesandordirs; Name: "{app}\Browser"

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}\Browser"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{userprograms}\overnet browser"; Filename: "{app}\Browser\overnet-browser.exe"; WorkingDir: "{app}\Browser"
Name: "{userdesktop}\overnet browser"; Filename: "{app}\Browser\overnet-browser.exe"; WorkingDir: "{app}\Browser"; Tasks: desktopicon

[Run]
Filename: "{app}\Browser\overnet-browser.exe"; Description: "{cm:LaunchNow}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; Профиль (закладки, история) удаляется только по согласию — см. [Code].
Type: filesandordirs; Name: "{app}\Browser"

[Code]
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Msg: String;
begin
  if (CurUninstallStep = usPostUninstall) and DirExists(ExpandConstant('{app}\Data')) then
  begin
    if ActiveLanguage = 'ru' then
      Msg := 'Удалить и профиль браузера (закладки, история, настройки)?'
    else
      Msg := 'Also delete the browser profile (bookmarks, history, settings)?';
    if (not UninstallSilent) and (MsgBox(Msg, mbConfirmation, MB_YESNO) = IDYES) then
      DelTree(ExpandConstant('{app}'), True, True, True);
  end;
end;
