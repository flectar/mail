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
; Tell Windows about the mail link handler below on install and uninstall.
ChangesAssociations=yes

[Files]
Source: "..\..\target\package\windows\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

; Offered as the default email app in Settings > Apps > Default apps. Only
; the user can make it the default, so the shared mailto key is left alone.
; The link is the whole command line after the option, as
; src/mailto_links.rs requires.
[Registry]
Root: HKA; Subkey: "Software\Classes\FlectarMail.Url.mailto"; ValueType: string; ValueName: ""; ValueData: "Flectar Mail email link"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\FlectarMail.Url.mailto"; ValueType: string; ValueName: "FriendlyTypeName"; ValueData: "Flectar Mail email link"
Root: HKA; Subkey: "Software\Classes\FlectarMail.Url.mailto\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\flectar-mail.exe"",0"
Root: HKA; Subkey: "Software\Classes\FlectarMail.Url.mailto\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\flectar-mail.exe"" --mailto ""%1"""
Root: HKA; Subkey: "Software\Clients\Mail\Flectar Mail"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Clients\Mail\Flectar Mail\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "Flectar Mail"
Root: HKA; Subkey: "Software\Clients\Mail\Flectar Mail\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Native, offline-first email client"
Root: HKA; Subkey: "Software\Clients\Mail\Flectar Mail\Capabilities"; ValueType: string; ValueName: "ApplicationIcon"; ValueData: """{app}\flectar-mail.exe"",0"
Root: HKA; Subkey: "Software\Clients\Mail\Flectar Mail\Capabilities\URLAssociations"; ValueType: string; ValueName: "mailto"; ValueData: "FlectarMail.Url.mailto"
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "Flectar Mail"; ValueData: "Software\Clients\Mail\Flectar Mail\Capabilities"; Flags: uninsdeletevalue

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
