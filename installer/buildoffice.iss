; Buildoffice の Windows 用のインストーラー（Inno Setup 6 の設定）。
;
; scripts/build-release.sh が、まとめたフォルダ（zip と同じ中身）から作る。手で作るときは、リポジトリの根で：
;   ISCC.exe /DAppVersion=0.2.0-preview /DWinVersion=0.2.0.0 /DStageDir=<まとめたフォルダの絶対パス> installer\buildoffice.iss
;
; - ユーザーごとに入れる（管理者の確認は出ない）。入る場所は %LOCALAPPDATA%\Programs\Buildoffice
;   （場所を選ぶ画面はない）。中の配置は zip と同じ。
; - スタートメニューに「Buildoffice」を作る。デスクトップのショートカットは、選んだときだけ作る（既定は作らない）。
; - アンインストールで消えるのは、このインストーラーが入れたファイル・ショートカット・登録だけ。保管庫（利用者の
;   ファイル）と、入れた場所に利用者があとから置いたものは消さない。そのために [UninstallDelete] と
;   [InstallDelete] は書かない。
; - AI のクライアントへの MCP の登録はしない（README.md で案内する）。PATH も変えない。
; - AppId は固定の GUID（公開のあとは変えない。変えると、更新とアンインストールが、前に入れたものを見つけられない）。
;   中括弧の「{{」は、Inno Setup の書き方で、文字の「{」になる（AppId は {GUID} の形になる）。
; - 入る場所は、前に入れた場所を引き継がず、いつも {autopf}\Buildoffice（UsePreviousAppDir=no）。
; - 画面は英語と日本語（Inno Setup に付いている訳）。Inno Setup の著作権の表示（About など）は消さない。

#ifndef AppVersion
  #error AppVersion を渡す（例：/DAppVersion=0.2.0-preview）
#endif
#ifndef WinVersion
  #error WinVersion を渡す（例：/DWinVersion=0.2.0.0）
#endif
#ifndef StageDir
  #error StageDir を渡す（まとめたフォルダの絶対パス）
#endif

[Setup]
AppId={{952E14BE-EEF3-4C9A-B454-D5555E71B1B3}
AppName=Buildoffice
AppVersion={#AppVersion}
AppPublisher=The Buildoffice Authors
AppPublisherURL=https://github.com/buildersloop/buildoffice
AppCopyright=Copyright (C) 2026 The Buildoffice Authors
VersionInfoVersion={#WinVersion}
VersionInfoProductName=Buildoffice
VersionInfoDescription=Buildoffice Setup
PrivilegesRequired=lowest
DefaultDirName={autopf}\Buildoffice
UsePreviousAppDir=no
DisableDirPage=yes
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
WizardStyle=modern
ShowLanguageDialog=auto
UninstallDisplayIcon={app}\app\vault_app.exe
CloseApplications=yes
RestartApplications=no
OutputDir=..\dist
OutputBaseFilename=Buildoffice-{#AppVersion}-windows-x64-setup
#ifdef Uncompressed
; 確かめ用（scripts/build-release.sh が作って検査し、捨てる。配らない）：中身を縮めずに入れて、検査で読めるようにする
Compression=none
InternalCompressLevel=none
SolidCompression=no
#else
Compression=lzma2/max
SolidCompression=yes
#endif

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "ja"; MessagesFile: "compiler:Languages\Japanese.isl"

[Messages]
en.FinishedLabel=Setup has finished installing [name]. You can start it from "Buildoffice" in the Start menu.%n%nUninstalling Buildoffice does not remove your vaults or the files in them.%n%nTo use your vault from an AI assistant (MCP), see README.md in the installation folder.
en.FinishedLabelNoIcons=Setup has finished installing [name].%n%nUninstalling Buildoffice does not remove your vaults or the files in them.%n%nTo use your vault from an AI assistant (MCP), see README.md in the installation folder.
en.ConfirmUninstall=Are you sure you want to remove %1?%n%nYour vaults and the files in them are not removed.
ja.FinishedLabel=[name] のインストールが終わりました。スタートメニューの「Buildoffice」から起動できます。%n%nアンインストールしても、保管庫とその中のファイルは消えません。%n%nAI（MCP）から保管庫を使うための登録のしかたは、インストールしたフォルダの README.md にあります。
ja.FinishedLabelNoIcons=[name] のインストールが終わりました。%n%nアンインストールしても、保管庫とその中のファイルは消えません。%n%nAI（MCP）から保管庫を使うための登録のしかたは、インストールしたフォルダの README.md にあります。
ja.ConfirmUninstall=%1 をアンインストールします。よろしいですか？%n%n保管庫とその中のファイルは消えません。

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Buildoffice"; Filename: "{app}\app\vault_app.exe"; WorkingDir: "{app}\app"
Name: "{autodesktop}\Buildoffice"; Filename: "{app}\app\vault_app.exe"; WorkingDir: "{app}\app"; Tasks: desktopicon

[Run]
Filename: "{app}\app\vault_app.exe"; Description: "{cm:LaunchProgram,Buildoffice}"; WorkingDir: "{app}\app"; Flags: nowait postinstall skipifsilent
