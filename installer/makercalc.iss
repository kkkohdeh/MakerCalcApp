; Inno Setup script for MakerCalc (Windows x64).
; Install per user in %LOCALAPPDATA% without administrator elevation.

#define MyAppName "MakerCalc"
#define MyAppPublisher "MakerCalc"
#define MyAppURL "https://makercalc.app"
#define MyAppExeName "makercalc.exe"
#ifndef MyAppBinary
  #define MyAppBinary "..\target\release\makercalc.exe"
#endif
#define MyAppVersion GetStringFileInfo(MyAppBinary, "ProductVersion")

[Setup]
AppId={{D37F2C58-6729-4A9B-B1D6-8E3D7B1C8891}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
VersionInfoVersion={#GetVersionNumbersString(MyAppBinary)}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={localappdata}\Programs\MakerCalc
DisableDirPage=yes
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
OutputDir=..\target\installer
OutputBaseFilename=MakerCalc-Setup
SetupIconFile=..\app_icon.ico
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.18362
CloseApplications=yes
RestartApplications=no
UninstallDisplayIcon={app}\{#MyAppExeName}

[Languages]
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"
Name: "startup"; Description: "{cm:StartupDescription}"; GroupDescription: "{cm:StartupGroup}"; Flags: unchecked checkedonce checkablealone; Check: ShouldOfferStartup
Name: "startup\background"; Description: "{cm:StartupBackground}"; GroupDescription: "{cm:StartupGroup}"; Flags: unchecked checkedonce dontinheritcheck

[CustomMessages]
english.StartupDescription=Start MakerCalc when signing in to Windows
spanish.StartupDescription=Iniciar MakerCalc con Windows
english.StartupBackground=Start in the background
spanish.StartupBackground=Iniciar en segundo plano
english.StartupGroup=Startup
spanish.StartupGroup=Inicio
english.StartupSetupFailed=Could not configure automatic startup. You can try again from MakerCalc Settings.
spanish.StartupSetupFailed=No se pudo configurar el inicio automático. Puedes volver a intentarlo desde Ajustes de MakerCalc.
english.ProfileCheckFailed=Could not check whether MakerCalc is using its profile.
spanish.ProfileCheckFailed=No se pudo comprobar si MakerCalc está usando su perfil.
english.AppStillOpen=MakerCalc is still open or did not respond. Choose Quit from its tray icon next to the clock, then uninstall again.
spanish.AppStillOpen=MakerCalc sigue abierta o no respondió. Elige Salir desde su icono junto al reloj y vuelve a desinstalar.
english.StartupRemovalFailed=Could not remove MakerCalc from Windows startup. You can disable it in Settings > Apps > Startup.
spanish.StartupRemovalFailed=No se pudo retirar MakerCalc del inicio de Windows. Puedes deshabilitarla en Configuración > Aplicaciones > Inicio.

[Registry]
Root: HKCU; Subkey: "Software\Classes\makercalc-notify"; ValueType: string; ValueName: ""; ValueData: "URL:MakerCalc notification"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\makercalc-notify"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\makercalc-notify\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" --notification ""%1"""
Root: HKCU; Subkey: "Software\Classes\AppUserModelId\MakerCalc.Desktop"; ValueType: string; ValueName: "DisplayName"; ValueData: "{#MyAppName}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\AppUserModelId\MakerCalc.Desktop"; ValueType: string; ValueName: "IconUri"; ValueData: "{app}\app_icon.ico"

[Files]
Source: "{#MyAppBinary}"; DestDir: "{app}"; DestName: "{#MyAppExeName}"; Flags: ignoreversion
Source: "..\target\installer\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\app_icon.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; AppUserModelID: "MakerCalc.Desktop"; AppUserModelToastActivatorCLSID: "F57125E9-38C1-4C89-A7A0-1FA5BC8E1734"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent

[Code]
#if VER < EncodeVer(7, 0, 0)
type
  { Inno 6 runs a 32-bit Setup/Uninstall even in 64-bit install mode. }
  NativeUInt = LongWord;
  NativeInt = Integer;
#endif
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  RunValue = 'MakerCalc';
  WindowMarker = 'MakerCalc.Desktop.Instance.v1';
  QuitMessage = 'MakerCalc.Desktop.Quit.v1';
  InstanceMarker = $4D43;

function FindWindowExW(Parent, Child: HWND; ClassName, WindowName: NativeUInt): HWND;
  external 'FindWindowExW@user32.dll stdcall';
function GetPropW(Window: HWND; Name: String): NativeUInt;
  external 'GetPropW@user32.dll stdcall';
function GetWindowThreadProcessId(Window: HWND; var ProcessId: Cardinal): Cardinal;
  external 'GetWindowThreadProcessId@user32.dll stdcall';
function RegisterWindowMessageW(Name: String): Cardinal;
  external 'RegisterWindowMessageW@user32.dll stdcall';
function SendMessageTimeoutW(Window: HWND; Msg: Cardinal; WParam: NativeUInt;
  LParam: NativeInt; Flags, Timeout: Cardinal; var Reply: NativeUInt): NativeInt;
  external 'SendMessageTimeoutW@user32.dll stdcall';
function OpenProcess(Access: Cardinal; Inherit: Integer; ProcessId: Cardinal): THandle;
  external 'OpenProcess@kernel32.dll stdcall';
function QueryFullProcessImageNameW(Process: THandle; Flags: Cardinal;
  FileName: String; var Size: Cardinal): Integer;
  external 'QueryFullProcessImageNameW@kernel32.dll stdcall';
function WaitForSingleObject(Handle: THandle; Timeout: Cardinal): Cardinal;
  external 'WaitForSingleObject@kernel32.dll stdcall';
function CloseHandle(Handle: THandle): Integer;
  external 'CloseHandle@kernel32.dll stdcall';
function CreateFileW(FileName: String; Access, Share: Cardinal; Security: NativeUInt;
  Creation, Attributes: Cardinal; Template: THandle): THandle;
  external 'CreateFileW@kernel32.dll stdcall';

function ShouldOfferStartup(): Boolean;
begin
  Result := not WizardSilent and not RegValueExists(HKCU, RunKey, RunValue);
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Argument: String;
  ResultCode: Integer;
begin
  if (CurStep <> ssPostInstall) or not ShouldOfferStartup() or
    not WizardIsTaskSelected('startup') then Exit;
  if WizardIsTaskSelected('startup\background') then
    Argument := '--setup-startup=background'
  else
    Argument := '--setup-startup=window';
  if not Exec(ExpandConstant('{app}\{#MyAppExeName}'), Argument, '', SW_HIDE,
    ewWaitUntilTerminated, ResultCode) or (ResultCode <> 0) then begin
    Log(Format('MakerCalc startup configuration failed (%d).', [ResultCode]));
    SuppressibleMsgBox(CustomMessage('StartupSetupFailed'), mbError, MB_OK, IDOK);
  end;
end;

function ProfileBusy(): Boolean;
var
  Handle: THandle;
  Error: Cardinal;
begin
  { A read-only, exclusive open checks whether the app still owns its profile lock file. }
  Handle := CreateFileW(ExpandConstant('{localappdata}\MakerCalc\instance.lock'),
    0, 0, 0, 3, $80, 0);
  if Handle <> THandle(-1) then begin
    CloseHandle(Handle);
    Result := False;
  end else begin
    Error := DLLGetLastError;
    if (Error <> 2) and (Error <> 3) and (Error <> 32) then
      RaiseException(CustomMessage('ProfileCheckFailed'));
    Result := Error = 32;
  end;
end;

function RequestInstalledInstanceQuit(var Found: Boolean): Boolean;
var
  Window: HWND;
  Process: THandle;
  Pid, Size, Msg: Cardinal;
  Reply: NativeUInt;
  FileName: String;
begin
  Result := False;
  Found := False;
  Window := FindWindowExW(0, 0, 0, 0);
  while Window <> 0 do begin
    if GetPropW(Window, WindowMarker) = InstanceMarker then begin
      GetWindowThreadProcessId(Window, Pid);
      Process := OpenProcess($101000, 0, Pid); { SYNCHRONIZE | QUERY_LIMITED_INFORMATION }
      if Process <> 0 then begin
        try
          Size := 32768;
          SetLength(FileName, Size);
          if QueryFullProcessImageNameW(Process, 0, FileName, Size) <> 0 then begin
            SetLength(FileName, Size);
            { Never close a dev build or a different installation/user. }
            if CompareText(FileName, ExpandConstant('{app}\{#MyAppExeName}')) = 0 then begin
              Found := True;
              Msg := RegisterWindowMessageW(QuitMessage);
              Reply := 0;
              if (Msg <> 0) and
                (SendMessageTimeoutW(Window, Msg, 0, 0, 3, 1000, Reply) <> 0) and
                (Reply = 1) then
                Result := WaitForSingleObject(Process, 14000) = 0;
              Exit;
            end;
          end;
        finally
          CloseHandle(Process);
        end;
      end;
    end;
    Window := FindWindowExW(0, Window, 0, 0);
  end;
end;

function InitializeUninstall(): Boolean;
var
  Attempt: Integer;
  Found: Boolean;
begin
  Result := False;
  { Permit a short startup race, but never kill the process or remove an in-use EXE. }
  for Attempt := 1 to 20 do begin
    if not ProfileBusy() then begin
      Result := True;
      Exit;
    end;
    if RequestInstalledInstanceQuit(Found) then begin
      Result := not ProfileBusy();
      if Result then Exit;
    end;
    if Found then Break;
    Sleep(100);
  end;
  MsgBox(CustomMessage('AppStillOpen'), mbError, MB_OK);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  { Independent of the installer task: it may have been enabled later from Settings. }
  if (CurUninstallStep = usPostUninstall) and RegValueExists(HKCU, RunKey, RunValue) then
    if not RegDeleteValue(HKCU, RunKey, RunValue) then
      MsgBox(CustomMessage('StartupRemovalFailed'), mbError, MB_OK);
end;
