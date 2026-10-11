; Inno Setup script for the Windows installer.
;
; Per user on purpose: Vigil is a single executable without services or drivers,
; and actions that need administrator rights ask for them through UAC when used.
; {autopf} therefore resolves to %LOCALAPPDATA%\Programs.
;
; Build with:  iscc /DAppVersion=0.3.0 vigil.iss
; Expects vigil.exe next to this file.

#define AppName "Vigil Task Manager"
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
; Never change AppId: it lets a new release replace an installed one in place.
AppId={{8E1C5B27-4A3F-4D9E-B6A2-7C0F1E93D584}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
VersionInfoVersion={#AppVersion}
AppPublisher=Vigil
AppPublisherURL=https://github.com/baba537/Vigil-Task-Manager
DefaultDirName={autopf}\Vigil
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
OutputBaseFilename=VigilSetup-{#AppVersion}
OutputDir=.
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
SetupIconFile=..\..\assets\vigil.ico
UninstallDisplayIcon={app}\vigil.exe
UninstallDisplayName={#AppName}
LicenseFile=..\..\LICENSE
; A running Vigil is closed through the Restart Manager instead of failing on a locked file.
CloseApplications=yes
RestartApplications=no

[Files]
Source: "vigil.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\vigil.exe"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\vigil.exe"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Run]
Filename: "{app}\vigil.exe"; Description: "Start {#AppName}"; \
  Flags: nowait postinstall skipifsilent

; Settings and app history in %APPDATA%\io.github.baba537.Vigil are the user's data
; and stay in place, so a reinstall keeps them.
