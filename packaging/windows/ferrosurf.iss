; Inno Setup script for FerroSurf. Build with:  iscc /DAppVersion=0.1.0 packaging\windows\ferrosurf.iss
; Expects the release binary at ..\..\target\release\ferrosurf.exe and resources in crates\ferrosurf\resources.
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#define AppName "FerroSurf"
#define AppExe "ferrosurf.exe"
#define RegKey "Software\FerroSurf"

[Setup]
AppId={{6B1F3E0A-4C2D-4F7B-9E55-F3770A11D5C2}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=FerroApps
DefaultDirName={autopf}\FerroSurf
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
OutputDir=..\..\dist
OutputBaseFilename=FerroSurf-{#AppVersion}-setup
SetupIconFile=..\..\crates\ferrosurf\resources\icons\ferrosurf.ico
UninstallDisplayIcon={app}\{#AppExe}
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
WizardStyle=modern

[Languages]
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\crates\ferrosurf\resources\*"; DestDir: "{app}\resources"; Flags: ignoreversion recursesubdirs createallsubdirs
; Optional runtime DLLs / extra files shipped next to the exe
Source: "..\..\target\release\*.dll"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; --- Default Programs registration (Capabilities) ---
Root: HKLM; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "FerroSurf"; ValueData: "Software\Clients\StartMenuInternet\FerroSurf\Capabilities"; Flags: uninsdeletevalue
Root: HKLM; Subkey: "Software\Clients\StartMenuInternet\FerroSurf"; ValueType: string; ValueName: ""; ValueData: "FerroSurf"; Flags: uninsdeletekey
Root: HKLM; Subkey: "Software\Clients\StartMenuInternet\FerroSurf\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},0"
Root: HKLM; Subkey: "Software\Clients\StartMenuInternet\FerroSurf\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"""
Root: HKLM; Subkey: "Software\Clients\StartMenuInternet\FerroSurf\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "FerroSurf"
Root: HKLM; Subkey: "Software\Clients\StartMenuInternet\FerroSurf\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Navegador rápido, moderno y nativo para IA"
Root: HKLM; Subkey: "Software\Clients\StartMenuInternet\FerroSurf\Capabilities"; ValueType: string; ValueName: "ApplicationIcon"; ValueData: "{app}\{#AppExe},0"
Root: HKLM; Subkey: "Software\Clients\StartMenuInternet\FerroSurf\Capabilities\URLAssociations"; ValueType: string; ValueName: "http"; ValueData: "FerroSurfURL"
Root: HKLM; Subkey: "Software\Clients\StartMenuInternet\FerroSurf\Capabilities\URLAssociations"; ValueType: string; ValueName: "https"; ValueData: "FerroSurfURL"
Root: HKLM; Subkey: "Software\Clients\StartMenuInternet\FerroSurf\Capabilities\FileAssociations"; ValueType: string; ValueName: ".html"; ValueData: "FerroSurfHTML"
Root: HKLM; Subkey: "Software\Clients\StartMenuInternet\FerroSurf\Capabilities\FileAssociations"; ValueType: string; ValueName: ".htm"; ValueData: "FerroSurfHTML"
; --- ProgIDs ---
Root: HKLM; Subkey: "Software\Classes\FerroSurfURL"; ValueType: string; ValueName: ""; ValueData: "FerroSurf URL"; Flags: uninsdeletekey
Root: HKLM; Subkey: "Software\Classes\FerroSurfURL"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKLM; Subkey: "Software\Classes\FerroSurfURL\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},0"
Root: HKLM; Subkey: "Software\Classes\FerroSurfURL\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKLM; Subkey: "Software\Classes\FerroSurfHTML"; ValueType: string; ValueName: ""; ValueData: "FerroSurf HTML Document"; Flags: uninsdeletekey
Root: HKLM; Subkey: "Software\Classes\FerroSurfHTML\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe},0"
Root: HKLM; Subkey: "Software\Classes\FerroSurfHTML\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
