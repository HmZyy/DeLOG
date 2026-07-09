; Build: iscc /DMyAppVersion=<ver> /DStagingDir=<abs path> packaging\windows\delog.iss
#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif
#ifndef StagingDir
  #define StagingDir "..\..\staging"
#endif

[Setup]
AppName=DeLOG
AppVersion={#MyAppVersion}
AppPublisher=HmZyy
DefaultDirName={autopf}\DeLOG
DefaultGroupName=DeLOG
UninstallDisplayIcon={app}\delog.exe
OutputDir=Output
OutputBaseFilename=delog-{#MyAppVersion}-windows-x86_64-bundled-setup
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Files]
Source: "{#StagingDir}\delog.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StagingDir}\python\*"; DestDir: "{app}\python"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\DeLOG"; Filename: "{app}\delog.exe"
Name: "{group}\Uninstall DeLOG"; Filename: "{uninstallexe}"

[Run]
Filename: "{app}\delog.exe"; Description: "Launch DeLOG"; Flags: nowait postinstall skipifsilent
