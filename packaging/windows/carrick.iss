; Carrick's Windows installer (Inno Setup 6). Built by packaging/build-windows.ps1,
; which passes the version and where the release programs are:
;   ISCC /DVersion=0.1.0 /DBinDir=<folder with carrick.exe> /DDistDir=<dist> carrick.iss
;
; By default it installs for the current user only, with no administrator
; rights, and puts the folder on that user's PATH so `carrick` works in any
; terminal. An administrator can choose to install it for everyone instead.

#ifndef Version
  #define Version "0.0.0"
#endif

[Setup]
AppId={{6D3B1C52-7A41-4E36-9B0E-2C8F4A1D7E05}
AppName=Carrick
AppVersion={#Version}
AppVerName=Carrick {#Version}
AppPublisher=Johan Socarras
DefaultDirName={autopf}\Carrick
DefaultGroupName=Carrick
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ChangesEnvironment=yes
OutputDir={#DistDir}
OutputBaseFilename=carrick-{#Version}-setup
SetupIconFile=..\carrick.ico
UninstallDisplayIcon={app}\carrick-gui.exe
LicenseFile=..\..\LICENSE-MIT
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#BinDir}\carrick.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\carrick-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\carrick.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BinDir}\THIRD-PARTY-LICENSES.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Carrick"; Filename: "{app}\carrick-gui.exe"; IconFilename: "{app}\carrick.ico"
Name: "{autodesktop}\Carrick"; Filename: "{app}\carrick-gui.exe"; IconFilename: "{app}\carrick.ico"; Tasks: desktopicon

[Registry]
; `carrick autostart on` writes this value; take it away on uninstall too.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Carrick"; Flags: dontcreatekey uninsdeletevalue

[Run]
Filename: "{app}\carrick-gui.exe"; Description: "{cm:LaunchProgram,Carrick}"; Flags: nowait postinstall skipifsilent

[Code]
// The PATH of the user (or of the machine, when installed for everyone).
function PathRoot: Integer;
begin
  if IsAdminInstallMode then Result := HKEY_LOCAL_MACHINE else Result := HKEY_CURRENT_USER;
end;

function PathKey: String;
begin
  if IsAdminInstallMode then
    Result := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
  else
    Result := 'Environment';
end;

function HasEntry(Path, Dir: String): Boolean;
begin
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Path) + ';') > 0;
end;

procedure AddToPath(Dir: String);
var
  Path: String;
begin
  if not RegQueryStringValue(PathRoot, PathKey, 'Path', Path) then Path := '';
  if HasEntry(Path, Dir) then exit;
  // Keep the PATH's own style, with or without a trailing ';', so that
  // RemoveFromPath gives back exactly what was there.
  if Path = '' then
    Path := Dir
  else if Copy(Path, Length(Path), 1) = ';' then
    Path := Path + Dir + ';'
  else
    Path := Path + ';' + Dir;
  RegWriteExpandStringValue(PathRoot, PathKey, 'Path', Path);
end;

procedure RemoveFromPath(Dir: String);
var
  Path, Padded: String;
  P: Integer;
begin
  if not RegQueryStringValue(PathRoot, PathKey, 'Path', Path) then exit;
  Padded := ';' + Path + ';';
  P := Pos(';' + Uppercase(Dir) + ';', Uppercase(Padded));
  if P = 0 then exit;
  Delete(Padded, P, Length(Dir) + 1);
  // Take away the padding added above.
  Path := Copy(Padded, 2, Length(Padded) - 2);
  RegWriteExpandStringValue(PathRoot, PathKey, 'Path', Path);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then AddToPath(ExpandConstant('{app}'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then RemoveFromPath(ExpandConstant('{app}'));
end;
