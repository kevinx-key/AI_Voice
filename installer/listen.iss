; 倾听输入法 二合一安装包（语音输入 + Rime 键盘输入）
;
; 编译：K:\app\InnoSetup\ISCC.exe installer\listen.iss
; 或直接跑 installer\build.ps1 -Setup（会带上 -DIncludeLM）
;
; 设计要点：
; - 提权安装（写 Program Files + 装倾听输入法本体）
; - 资源（倾听输入法安装器 + 四套方案包 + 万象语法模型）随包分发，全离线
; - 装完以「原始用户」身份拉起首启向导（不把常驻进程留在管理员权限下）
; - 卸载：先跑 listen.exe --rime-uninstall 清 Rime 侧文件（保留用户词库/配置），
;         再询问是否一并卸载倾听输入法本体

#define AppName "倾听输入法"
#define AppVer "0.1.1"
#define AppExe "listen.exe"
#define AppPublisher "kevin"

; 语法模型（约 400MB）：默认包含；build.ps1 -NoLM 会传 /DIncludeLM=0
#ifndef IncludeLM
  #define IncludeLM 1
#endif

[Setup]
AppId={{8F3C1A72-5B4E-4C9A-9E11-AIVOICE000001}
AppName={#AppName}
AppVersion={#AppVer}
AppPublisher={#AppPublisher}
DefaultDirName={autopf}\Listen
DefaultGroupName=倾听输入法
DisableProgramGroupPage=yes
OutputDir=..\dist
OutputBaseFilename=listen-{#AppVer}-setup
Compression=lzma2/fast
SolidCompression=no
WizardStyle=modern
PrivilegesRequired=admin
ArchitecturesInstallIn64BitMode=x64compatible
ArchitecturesAllowed=x64compatible
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
SetupIconFile=K:\K3R-voice\resource\k3r\ico\weasel.ico
AppMutex=Global\ListenRust
MinVersion=10.0

[Languages]
Name: "cn"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "附加任务："
Name: "autostart"; Description: "开机自启（随系统启动）"; GroupDescription: "附加任务："; Flags: unchecked

[Registry]
; App Paths：让倾听输入法（WeaselServer / WeaselSetup / WeaselDeployer）以及任何进程
; 都能用 ShellExecute("listen.exe", "--settings") 直接唤起 倾听输入法 设置窗。
; 本安装器以 x64 模式运行，写入的是 64 位视图（64 位进程才读得到）。
Root: HKLM; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\listen.exe"; ValueType: string; ValueName: ""; ValueData: "{app}\{#AppExe}"; Flags: uninsdeletekey

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
; 绿色配置：已有配置不覆盖（老用户升级时保留）
Source: "..\config.default.toml"; DestDir: "{app}"; DestName: "config.toml"; Flags: onlyifdoesntexist uninsneveruninstall
; 门控 VAD 模型（C1，Silero，~630KB）：随包带，免得装完首次说话还要联网下模型
Source: "..\resources\models\silero_vad.onnx"; DestDir: "{app}\models"; Flags: ignoreversion
; 倾听输入法官方安装器（锁定 0.17.4）
Source: "..\resources\rime\weasel-0.17.4.0-installer.exe"; DestDir: "{app}\resources\rime"; Flags: ignoreversion
; 三套方案包（雾凇拼音 / easy_en / 日文）
Source: "..\resources\rime\schemes\*"; DestDir: "{app}\resources\rime\schemes"; Flags: ignoreversion recursesubdirs createallsubdirs
; 万象语法模型（约 400MB；用 build.ps1 -NoLM 编译可排除）
#if IncludeLM
Source: "..\resources\rime\lm\*"; DestDir: "{app}\resources\rime\lm"; Flags: ignoreversion
#endif

[Icons]
Name: "{group}\倾听输入法 设置"; Filename: "{app}\{#AppExe}"; Parameters: "--settings"
Name: "{group}\卸载 倾听输入法"; Filename: "{uninstallexe}"
Name: "{autodesktop}\倾听输入法"; Filename: "{app}\{#AppExe}"; Parameters: "--settings"; Tasks: desktopicon

[Run]
; ① 静默安装输入法引擎本体（倾听输入法 Rime，随包携带）。/S 复用注册表里的旧安装目录，
;    这样"一个安装包"即可完成「语音输入 + 键盘输入」两部分，用户不必再单独装第二个包。
Filename: "{app}\resources\rime\weasel-0.17.4.0-installer.exe"; Parameters: "/S"; Flags: runhidden waituntilterminated; StatusMsg: "正在安装输入法引擎（倾听输入法 Rime）…"
; ② 首启向导：以原始（非提权）用户身份拉起，向导里选主力语言后点「立即部署」
Filename: "{app}\{#AppExe}"; Parameters: "--first-run"; Description: "打开首启向导（选主力语言并部署输入法）"; Flags: postinstall runasoriginaluser nowait skipifsilent

[UninstallRun]
; 先清 Rime 侧我们铺的文件（保留用户词库/配置）；runhidden 免得闪窗
Filename: "{app}\{#AppExe}"; Parameters: "--rime-uninstall"; Flags: runhidden waituntilterminated; RunOnceId: "RimeUninstall"

[Code]
var
  WeaselPage: TOutputMsgWizardPage;

procedure InitializeWizard;
begin
  WeaselPage := CreateOutputMsgPage(wpSelectTasks,
    '二合一说明',
    '安装包含两个部分：',
    '1) 语音输入（倾听输入法本体，约 33MB）' + #13#10 +
    '2) 键盘输入（倾听输入法 Rime + 三套离线方案包：中文雾凇拼音 / 英文 / 日文，约 622MB）' + #13#10#13#10 +
    '安装完成后会自动打开「首启向导」，选一种主力语言点「立即部署」即可。' + #13#10 +
    '首次部署要编译词库（日文约 94MB），可能需要几分钟。' + #13#10#13#10 +
    '卸载时只删我们铺的方案包，你的词库和个人配置都会保留。');
end;

function InitializeUninstall(): Boolean;
begin
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Res: Integer;
  WeaselUninst: String;
  Keep: Integer;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    // 倾听输入法本体：问一句，默认保留
    if RegQueryStringValue(HKLM32, 'SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Weasel', 'UninstallString', WeaselUninst) then
    begin
      Keep := MsgBox('是否一并卸载「倾听输入法」输入法本体？' + #13#10#13#10 +
        '选「是」= 卸载倾听输入法（你的词库和个人配置仍保留在 %APPDATA%\Rime）' + #13#10 +
        '选「否」= 保留倾听输入法（如果你还要用它输入中文/日文）',
        mbConfirmation, MB_YESNO);
      if Keep = IDYES then
      begin
        Exec(RemoveQuotes(WeaselUninst), '/S', '', SW_SHOW, ewWaitUntilTerminated, Res);
      end;
    end;
  end;
end;
