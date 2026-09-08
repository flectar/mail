; Compile with Inno Setup 6 after staging target/package/windows.
#ifndef AppVersion
  #error AppVersion must be supplied
#endif
#ifndef NativeVersion
  #error NativeVersion must be supplied
#endif

[Setup]
AppId={{B3F4C53F-77C7-4639-9938-C01A21F4A0B9}
AppName=Flectar Mail
AppVersion={#AppVersion}
VersionInfoVersion={#NativeVersion}
AppPublisher=Flectar
AppPublisherURL=https://flectar.com
DefaultDirName={localappdata}\Programs\Flectar Mail
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\flectar-mail.exe
SetupIconFile=..\..\resources\app-icon\flectar-mail.ico
OutputDir=..\..\target
OutputBaseFilename=flectar-mail-windows-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes

[Files]
Source: "..\..\target\package\windows\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{userprograms}\Flectar Mail"; Filename: "{app}\flectar-mail.exe"

[Run]
Filename: "{app}\flectar-mail.exe"; Description: "Open Flectar Mail"; Flags: nowait postinstall skipifsilent
