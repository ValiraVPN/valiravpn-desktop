; ValiraVPN — Windows setup wizard.
;
; Built with Inno Setup 6. From the repository root:
;
;   cargo build --release
;   "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\valira.iss
;
; The result lands in installer\out\ValiraVPN-<version>-setup.exe.
;
; Two things about this program shape the script. It runs elevated, because
; every route to a working tunnel is privileged — so the installer is per
; machine, into Program Files. And it lives in the notification area: closing
; its window only hides it, so an upgrade cannot simply wait for the window to
; go. `--quit` asks a running client to shut down properly, tunnel and all,
; which is what PrepareToInstall below calls.

#define AppName        "ValiraVPN"
#define AppPublisher   "ValiraVPN"
#define AppUrl         "https://valiravpn.com"
#define AppExe         "valira-desktop.exe"
#define SourceDir      "..\target\release"

; Read straight from the built binary, so the installer can never claim a
; version the executable does not carry.
#define AppVersion GetVersionNumbersString(SourceDir + "\" + AppExe)

[Setup]
AppId={{7B3F2C41-9A6E-4D18-B5C7-2E0A8D4F91C3}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}
AppUpdatesURL={#AppUrl}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
OutputDir=out
OutputBaseFilename={#AppName}-{#AppVersion}-setup
SetupIconFile=..\windows\valira.ico
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern

; Per machine, into Program Files: the client needs administrator rights to
; create a tunnel interface at all, so a per-user install would only be able to
; put itself somewhere it could never work from.
PrivilegesRequired=admin

; 64-bit only. The tunnel driver beside the executable is the amd64 build, and
; offering the install on a machine that cannot load it would fail later rather
; than here.
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "french"; MessagesFile: "compiler:Languages\French.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "startup"; Description: "Start {#AppName} when I sign in to Windows"; GroupDescription: "Startup"

[Files]
Source: "{#SourceDir}\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
; Loaded by name at run time from beside the executable. Without it the
; embedded tunnel cannot create an interface, and the client would be left
; asking the user to install WireGuard — which shipping this is meant to avoid.
Source: "{#SourceDir}\wintun.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\vendor\wintun\LICENSE.txt"; DestDir: "{app}"; DestName: "WINTUN-LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
; `runascurrentuser` matters: setup is elevated, and without it the client
; would inherit that token and start under whichever account ran the install.
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent runascurrentuser

[Registry]
; The client asks for administrator rights, so the ordinary Run key cannot
; start it — Windows will not raise a UAC prompt at sign-in. A scheduled task
; with the highest privileges is the supported way round that; it is created in
; code below, and this key is only removed if an older build left one.
Root: HKLM; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "{#AppName}"; ValueType: none; Flags: deletevalue uninsdeletevalue

[UninstallDelete]
Type: files; Name: "{app}\wintun.dll"

[Code]

const
  QuitPatienceMs = 15000;

function RunningClientPath: String;
begin
  Result := ExpandConstant('{app}\{#AppExe}');
end;

// Asks a client that is already running to shut down before its file is
// replaced. Closing the window is not enough — this one goes to the tray — and
// terminating the process would leave the tunnel up and its routes pinned. The
// executable answers `--quit` by shutting itself down through the ordinary
// exit path.
function StopRunningClient: Boolean;
var
  Code: Integer;
begin
  Result := True;
  if not FileExists(RunningClientPath) then
    Exit;
  if not Exec(RunningClientPath, '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code) then
    Exit;
  // A moment for the process to follow its window out.
  Sleep(400);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  NeedsRestart := False;
  if not StopRunningClient then
    Result := 'ValiraVPN is running and could not be closed. Right-click its icon in the notification area, choose Close, then run this installer again.';
end;

// Started at sign-in through a scheduled task rather than the Run key: the
// client requires administrator rights, and Windows silently refuses to start
// an elevated program from Run.
procedure CreateStartupTask;
var
  Code: Integer;
  Command: String;
begin
  Command := '/Create /F /TN "ValiraVPN" /SC ONLOGON /RL HIGHEST /TR """' + RunningClientPath + '"""';
  Exec(ExpandConstant('{sys}\schtasks.exe'), Command, '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

procedure RemoveStartupTask;
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\schtasks.exe'), '/Delete /F /TN "ValiraVPN"', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    // Rebuilt either way, so switching the option off on an upgrade removes it.
    RemoveStartupTask;
    if WizardIsTaskSelected('startup') then
      CreateStartupTask;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
  begin
    // The tunnel comes down with the client, before its files go.
    StopRunningClient;
    RemoveStartupTask;
  end;
end;
