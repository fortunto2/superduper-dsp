; SuperDuper DSP — Windows installer (Inno Setup).
;
; Lets the user pick which plugin format(s) to install:
;   - CLAP  → {commoncf}\CLAP   (C:\Program Files\Common Files\CLAP)
;   - VST3  → {commoncf}\VST3   (C:\Program Files\Common Files\VST3)
;
; VST3 is a clap-wrapper shell that loads the matching .clap at runtime, so
; selecting VST3 force-selects CLAP too (see the Components `Types`/checks).
;
; Compiled in CI:
;   iscc /DMyAppVersion=%V% installer\windows\superduper.iss
; expects the payload already staged under dist\ :
;   dist\clap\*.clap      — the renamed-.dll CLAP plugins
;   dist\vst3\*.vst3       — clap-wrapper VST3 bundles (folders)

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0-dev"
#endif

[Setup]
AppId={{7B1C0A4E-5D2F-4E8B-9C3A-SDSP00000001}
AppName=SuperDuper DSP
AppVersion={#MyAppVersion}
AppPublisher=SuperDuperAI
DefaultDirName={autopf}\SuperDuperAI\SuperDuper DSP
DisableDirPage=yes
DisableProgramGroupPage=yes
UninstallDisplayName=SuperDuper DSP {#MyAppVersion}
OutputDir={#SourcePath}..\..\dist
OutputBaseFilename=SuperDuperDSP-{#MyAppVersion}-windows-x64-Setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; Plugins are 64-bit — install into the 64-bit Common Files.
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin

[Types]
Name: "full";   Description: "CLAP + VST3 (recommended)"
Name: "clap";   Description: "CLAP only"
Name: "custom"; Description: "Custom"; Flags: iscustom

[Components]
; CLAP is the real plugin; always available.
Name: "clap"; Description: "CLAP plugins (REAPER, Bitwig, Studio One)"; Types: full clap custom; Flags: fixed
; VST3 is a wrapper that loads the CLAP at runtime → depends on clap.
Name: "vst3"; Description: "VST3 plugins (Cubase, Ableton, FL, DaVinci) — also installs CLAP"; Types: full custom

[Files]
; --- CLAP: single-file .dll renamed .clap ---
Source: "{#SourcePath}..\..\dist\clap\*.clap"; DestDir: "{commoncf}\CLAP"; Components: clap; Flags: ignoreversion
; --- VST3: bundle folders (Name.vst3\Contents\x86_64-win\Name.vst3) ---
Source: "{#SourcePath}..\..\dist\vst3\*";      DestDir: "{commoncf}\VST3"; Components: vst3; Flags: ignoreversion recursesubdirs createallsubdirs
; VST3 loads the .clap at runtime → make sure CLAP lands too even in a VST3-only pick.
Source: "{#SourcePath}..\..\dist\clap\*.clap"; DestDir: "{commoncf}\CLAP"; Components: vst3; Flags: ignoreversion

[Run]
Filename: "https://github.com/fortunto2/superduper-dsp"; Description: "Open project page"; Flags: postinstall shellexec skipifsilent unchecked

[UninstallDelete]
Type: filesandordirs; Name: "{commoncf}\CLAP\SuperDuper*.clap"
Type: filesandordirs; Name: "{commoncf}\VST3\SuperDuper*.vst3"
