; Compile with Inno Setup 6 after staging target/package/windows.
#ifndef AppVersion
  #error AppVersion must be supplied
#endif
#ifndef NativeVersion
  #error NativeVersion must be supplied
#endif
#ifndef Arch
  #error Arch must be supplied: x64 or arm64
#endif
; x64compatible also admits Arm64 Windows 11, which runs x64 apps under
; emulation; the arm64 installer is native there.
#if Arch == "x64"
  #define Architectures "x64compatible"
#elif Arch == "arm64"
  #define Architectures "arm64"
#else
  #error Arch must be x64 or arm64
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
ArchitecturesAllowed={#Architectures}
ArchitecturesInstallIn64BitMode={#Architectures}
MinVersion=10.0
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\flectar-mail.exe
SetupIconFile=..\..\resources\app-icon\flectar-mail.ico
OutputDir=..\..\target
OutputBaseFilename=flectar-mail-windows-{#Arch}-setup
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

[Code]
// The app registers its own per-user sign-in entry; remove it with the app so
// Windows does not keep a startup item that points at a deleted executable.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', 'Flectar Mail');
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run', 'Flectar Mail');
  end;
end;
