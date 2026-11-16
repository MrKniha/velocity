; Velocity setup (Inno Setup 6). Built by scripts\package.ps1, which passes the version, the staged files and the icon:
;   ISCC /DAppVersion=0.1.0 /DStage=<folder with velocity.exe, its DLLs and extensions> /DIcon=<velocity.ico> /O<output dir> velocity.iss
; Installs for the current user only (no admin prompt) into %LOCALAPPDATA%\Programs\Velocity, adds a Start menu entry
; (so Windows search finds it), an uninstaller in Settings > Apps, and registers Velocity as a browser so it can be
; picked in Settings > Default apps.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{6E7C2B1A-4F0D-4E8B-9C3E-5A1D2F7B9C40}
AppName=Velocity
AppVersion={#AppVersion}
AppVerName=Velocity {#AppVersion}
AppPublisher=Velocity
DefaultDirName={localappdata}\Programs\Velocity
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
OutputBaseFilename=Velocity-Setup-{#AppVersion}
SetupIconFile={#Icon}
UninstallDisplayIcon={app}\velocity.exe
UninstallDisplayName=Velocity
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
CloseApplications=yes
ChangesAssociations=yes

[Tasks]
Name: desktopicon; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"

[Files]
Source: "{#Stage}\velocity.exe"; DestDir: "{app}"; Flags: ignoreversion
; The engine's runtime (ANGLE, GStreamer): velocity.exe loads them from its own folder.
Source: "{#Stage}\*.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\README.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\extensions\*"; DestDir: "{app}\extensions"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{userprograms}\Velocity"; Filename: "{app}\velocity.exe"; Comment: "Browse the web with Velocity"
Name: "{userdesktop}\Velocity"; Filename: "{app}\velocity.exe"; Tasks: desktopicon

[Registry]
; Velocity as a browser (Settings > Default apps lists it; the user chooses it there, Windows doesn't allow apps to).
Root: HKCU; Subkey: "Software\Clients\StartMenuInternet\Velocity"; ValueType: string; ValueName: ""; ValueData: "Velocity"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Clients\StartMenuInternet\Velocity\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\velocity.exe,0"
Root: HKCU; Subkey: "Software\Clients\StartMenuInternet\Velocity\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\velocity.exe"""
Root: HKCU; Subkey: "Software\Clients\StartMenuInternet\Velocity\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "Velocity"
Root: HKCU; Subkey: "Software\Clients\StartMenuInternet\Velocity\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "A fast, light browser you can set up the way you like."
Root: HKCU; Subkey: "Software\Clients\StartMenuInternet\Velocity\Capabilities"; ValueType: string; ValueName: "ApplicationIcon"; ValueData: "{app}\velocity.exe,0"
Root: HKCU; Subkey: "Software\Clients\StartMenuInternet\Velocity\Capabilities\StartMenu"; ValueType: string; ValueName: "StartMenuInternet"; ValueData: "Velocity"
Root: HKCU; Subkey: "Software\Clients\StartMenuInternet\Velocity\Capabilities\URLAssociations"; ValueType: string; ValueName: "http"; ValueData: "VelocityURL"
Root: HKCU; Subkey: "Software\Clients\StartMenuInternet\Velocity\Capabilities\URLAssociations"; ValueType: string; ValueName: "https"; ValueData: "VelocityURL"
Root: HKCU; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "Velocity"; ValueData: "Software\Clients\StartMenuInternet\Velocity\Capabilities"; Flags: uninsdeletevalue
; How Windows opens a link with Velocity: velocity.exe "<address>".
Root: HKCU; Subkey: "Software\Classes\VelocityURL"; ValueType: string; ValueName: ""; ValueData: "Velocity link"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\VelocityURL"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\VelocityURL\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\velocity.exe,0"
Root: HKCU; Subkey: "Software\Classes\VelocityURL\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\velocity.exe"" ""%1"""

[Run]
Filename: "{app}\velocity.exe"; Description: "Open Velocity"; Flags: nowait postinstall skipifsilent
Filename: "ms-settings:defaultapps?registeredAppUser=Velocity"; Description: "Make Velocity my default browser (opens Windows Settings)"; Flags: shellexec nowait postinstall skipifsilent unchecked
; An update Velocity started itself (src/update.rs, /UPDATE=1): open it again once installed.
Filename: "{app}elocity.exe"; Flags: nowait; Check: IsUpdate

[Code]
function IsUpdate: Boolean;
begin
  Result := ExpandConstant('{param:UPDATE|0}') = '1';
end;

; Settings, history and profiles in %LOCALAPPDATA%\velocity are kept on uninstall, like other browsers do.
