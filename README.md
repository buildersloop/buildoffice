# Buildoffice

**Preview release 0.2.0 (Phase 0) — Windows x64**

[English](#english) | [日本語](#日本語)

---

## English

### What is Buildoffice?

Buildoffice is a local-first, open-source workspace. Your files live in a folder on your own PC
called a **vault**. The long-term goal is to sync vaults between your devices and to edit documents,
spreadsheets and slides together in real time, with every operation also available to AI assistants
through the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/).

This preview is **Phase 0**, the foundation:

- a Rust core that manages a vault and detects file changes,
- an MCP server (`vault-mcp.exe`) so that an AI assistant can read and write files in the vault,
- a small desktop app (`vault_app.exe`) to browse the vault.

The app's screens and the comments in the source code are currently in Japanese.
The messages of `vault-mcp.exe` (its `--help`, the output of `init`, and the error texts returned to the
AI over MCP) are also in Japanese only.

### What you can do in this preview

- **Start with a ready vault.** Each time the app starts, it opens the **default vault**: a folder named
  `Buildoffice` in your home folder (`%USERPROFILE%\Buildoffice`). If that folder does not exist, the app creates
  it as a new, empty vault. See "The default vault" below.
- **Create and open other vaults** (an empty folder, or a folder that does not exist yet).
- **Let an AI read and write files through MCP.** `vault-mcp.exe` is a stdio MCP server with ten tools:
  `vault_info`, `vault_list`, `vault_read`, `vault_write`, `vault_mkdir`, `vault_move`, `vault_delete`,
  `vault_trash_list`, `vault_restore`, `vault_changes`. Use `--read-only` to allow reading only.
- **Browse files in the app**, create files and folders, and move things to the trash.
- **Restore from the trash.** Deleted and overwritten files go to the vault's trash and can be restored,
  from the app or with `vault_restore`.
- **See changes made by other apps.** `vault_changes` (and the app's refresh) report files created,
  modified, deleted or renamed outside Buildoffice.

### Not yet available

Sync between devices, real-time co-editing, document/spreadsheet/slide editors, mail and calendar,
and macOS/Linux/mobile builds are **not** part of this preview.

Planned stages:

1. **Phase 1:** sync between your own devices (end-to-end encrypted; self-hostable server; mesh VPN).
2. **Phase 2:** documents and real-time co-editing, with AI as a co-editing participant.
3. **Phase 3:** spreadsheets (formula engine).
4. **Phase 4:** slides, mail and calendar.

### Using the preview on Windows

Requirements:

- Windows 10 or 11, x64.

#### Install with the setup program (recommended)

1. **Download** `Buildoffice-0.2.0-preview-windows-x64-setup.exe` from the GitHub Releases page.
   You can compare its SHA-256 with the value on the release page:
   `certutil -hashfile Buildoffice-0.2.0-preview-windows-x64-setup.exe SHA256`.
   A matching SHA-256 only shows that the file was downloaded without damage; it does not show who
   made the file.
2. **Run it.** The setup program installs for your Windows user only, so no administrator prompt appears.
   It installs into `%LOCALAPPDATA%\Programs\Buildoffice` (there is no screen to choose the folder) and adds
   a "Buildoffice" shortcut to the Start menu. A desktop shortcut is optional and off by default. The setup
   screens are in English or Japanese, following your Windows display language. It does **not** change your
   `PATH`, and it does **not** register anything with an AI client.
3. **Start Buildoffice from the Start menu.** It opens the default vault `%USERPROFILE%\Buildoffice`. The first
   time, it creates this vault and shows "保管庫を作った（ホームのフォルダの中の Buildoffice）" (the vault was created).
   To make another vault, close the vault with "×" at the top right and use the "新しく作る" (create new) card.
   Step 2 of "Using the zip instead" below explains the form and the places that are refused.
4. **Register the MCP server with your AI client.** For Claude Code, in Command Prompt (this registers the
   default vault):

   ```bat
   claude mcp add buildoffice -- "%LOCALAPPDATA%\Programs\Buildoffice\vault-mcp.exe" --vault "%USERPROFILE%\Buildoffice" --read-only
   ```

   In PowerShell, write `$env:LOCALAPPDATA` and `$env:USERPROFILE` instead of `%LOCALAPPDATA%` and `%USERPROFILE%`
   (for example `--vault "$env:USERPROFILE\Buildoffice"`). For another vault, pass its absolute path to `--vault`.
   Start the app once before you register the default vault, so that it exists. Remove `--read-only` when you
   want the AI to be able to write. Register with the `local` (default) or `user` scope; do not use the `project`
   scope, because it would put your user name and your vault's absolute path into a shared `.mcp.json`.
   Other MCP clients: register a stdio server with command `vault-mcp.exe` (its full path, as above) and
   arguments `--vault <absolute path of the vault>`.

**Updating.** Close the app and your AI client first (an AI client may be running `vault-mcp.exe`), then open
the new setup program. It installs over the old version, and your vaults are not touched. If a program is
still running, the setup program asks whether to close it.

**Uninstalling.** First remove the registration from your AI client: `claude mcp remove buildoffice`. Then open
Settings → Apps → Buildoffice → Uninstall. Only the files that the setup program installed are removed. Your
vaults (including the default vault `%USERPROFILE%\Buildoffice`) and the files in them are **not** deleted. A file
you put into the install folder yourself also stays.

#### Using the zip instead (no installation)

The zip has the same programs in the same layout. Extract it anywhere and run the programs from there.

1. **Download and extract** `Buildoffice-0.2.0-preview-windows-x64.zip` from the GitHub Releases page,
   for example to `C:\Tools`. The zip contains one folder, so this creates
   `C:\Tools\Buildoffice-0.2.0-preview-windows-x64\` (the examples below use this path).
   You can compare the zip's SHA-256 with the value on the release page:
   `certutil -hashfile Buildoffice-0.2.0-preview-windows-x64.zip SHA256`.
   A matching SHA-256 only shows that the file was downloaded without damage; it does not show who
   made the file.
2. **Start the app.** Run `C:\Tools\Buildoffice-0.2.0-preview-windows-x64\app\vault_app.exe`. It opens the default
   vault `%USERPROFILE%\Buildoffice` (the first time, it creates it), the same as the installed app.

   **To create another vault**, close the vault with "×" at the top right. The screen has three cards: the
   default vault at the top, "保管庫を開く" (open a vault) and "新しく作る" (create new). In the lowest card,
   "新しく作る":
   - **置き場所** (location): choose the folder to put the vault in with "選ぶ…" (choose…), or type its absolute
     path (for example `D:\Vaults`).
   - **保管庫の名前** (vault name): type a name for the vault (for example `MyVault`).
   - Press **"作って開く"** (create and open). The app makes a new folder with that name inside the location
     (here `D:\Vaults\MyVault`) and opens it.

   The location must be an absolute path that starts with a drive letter, and the vault folder must not exist
   yet or be empty. These places are refused on purpose: network locations (`\\server\...`), the root of a
   drive, your home folder itself (and the folders above it), anything under AppData, under the Windows
   folder, under Program Files, Program Files (x86) or ProgramData, the PowerShell profile folders
   (`Documents\PowerShell`, `Documents\WindowsPowerShell`), and folders inside another vault. The usual
   temporary folder is under AppData, so it is refused; a temporary folder somewhere else (for example
   `C:\Temp`) is not refused by this rule. A name cannot contain `\ / : * ? " < > |` or invisible characters,
   cannot have `~` followed by a digit (such as `file~1`), cannot end with `.` or a space, and cannot be a
   reserved name such as `CON` or `.vault`.

   To open that vault next time, close the default vault and use the card "保管庫を開く" (open a vault): choose
   the vault folder with "選ぶ…" or type its absolute path, then press "開く" (open). "読み取り専用で開く" opens it
   read-only. The app does not remember paths (it always starts with the default vault). The trash is at the
   top right.

   Instead of the app, you can also create a vault in Command Prompt:

   ```bat
   C:\Tools\Buildoffice-0.2.0-preview-windows-x64\vault-mcp.exe init D:\Vaults\MyVault
   ```
3. **Register the MCP server with your AI client.** For Claude Code, in Command Prompt (this registers the
   default vault; for another vault, pass its absolute path to `--vault`):

   ```bat
   claude mcp add buildoffice -- C:\Tools\Buildoffice-0.2.0-preview-windows-x64\vault-mcp.exe --vault "%USERPROFILE%\Buildoffice" --read-only
   ```

   In PowerShell, write `"$env:USERPROFILE\Buildoffice"` instead of `"%USERPROFILE%\Buildoffice"`.
   Remove `--read-only` when you want the AI to be able to write. Register with the `local` (default) or
   `user` scope; do not use the `project` scope, because it would put your vault's absolute path (which may
   contain your user name) into a shared `.mcp.json`. Other MCP clients: register a stdio server with
   command `vault-mcp.exe` and arguments `--vault <absolute path of the vault>`.

### The default vault

- **Where:** a folder named `Buildoffice` directly in your home folder, `%USERPROFILE%\Buildoffice`.
- **When:** each time the app starts, it opens this vault for reading and writing. If the folder does not
  exist, the app creates a new, empty vault there. The app does not remember which vault you used last.
  After you close a vault, the app does not reopen the default vault by itself; press "既定の保管庫を開く"
  (open the default vault) in the top card.
- **When it is not used:** if `Buildoffice` is a folder that is not a vault (empty or not), a file, or a link
  (a junction or a symbolic link, even one that points to a vault), the app does not create or open anything
  there. The top card "既定の保管庫" (default vault) shows the reason. Rename the item or move it away, then
  press "既定の保管庫を開く".
- **If you delete it:** the app creates a new, empty vault there at the next start. To get the deleted folder
  back from the Recycle Bin, close the app first, move away the new empty `Buildoffice` vault that the app made,
  and then restore the folder.
- **Uninstalling** does not delete the default vault.
- **Upgrading from 0.1.0:** after the update, the app opens the default vault at startup (and creates it the
  first time). Your existing vaults are not changed; open them with the card "保管庫を開く" (open a vault).

### Windows SmartScreen warning

The preview binaries are **not code-signed**, so Windows SmartScreen may show "Windows protected your PC"
when you run the setup program or start the programs. If you downloaded the setup program or the zip from
this project's GitHub Releases page and its SHA-256 matches, choose **More info → Run anyway**. If you are
unsure, build from source instead (below).

### Safety notes

- **This is a preview.** It has been tested, but it may still have bugs. Keep backups of important data,
  and do not keep the only copy of anything important in a vault yet.
- **It is designed not to touch anything outside the vault.** Every path is checked against the vault
  folder; links (junctions and symbolic links) are not followed out of the vault; deleting and overwriting
  move the old contents to the vault's trash instead of erasing them; and the MCP server cannot create a
  vault anywhere — only you can, with `vault-mcp.exe init` or the app (the app also creates the default vault
  `%USERPROFILE%\Buildoffice` when it starts and the folder does not exist).
- **These protections apply only to operations that go through Buildoffice's MCP tools.** An AI client's
  own shell or file tools are not covered: restrict them with the client's own permission settings.
- Other programs running as the same Windows user can still change the vault's files directly; that is
  outside what Buildoffice can prevent.
- Do not edit the `.vault` folder inside a vault by hand. It holds the history, the index and the trash.
- `--read-only` hides all writing tools from the AI. Note that `vault_changes` still updates the history
  and index under `.vault`.

### Building from source

Requirements:

- Windows 10/11 x64 and Git for Windows (the scripts run in **Git Bash**)
- Rust stable (MSVC toolchain), 1.89 or newer (tested with 1.100.0)
- Visual Studio 2022 Build Tools with "Desktop development with C++"
- Flutter 3.38 (Dart 3.10)
- Python 3 (only for the checks that the release script runs before it builds the zip)
- Inno Setup 6 (<https://jrsoftware.org/>), only to build the setup program (see below)

In Git Bash, at the root of the repository:

```sh
cargo test --workspace                         # tests of the Rust core
cargo build --release -p vault-mcp -p vault-ffi
cd app
flutter test
flutter build windows --release                # copies ../target/release/vault_ffi.dll next to the exe
```

The tests do not create anything in your home folder. Starting an app that you built (or `flutter run`) opens
and, if needed, creates the default vault `%USERPROFILE%\Buildoffice`, the same as the released app.

To build the release zip and the setup program with the same steps as the published ones:

```sh
scripts/build-release.sh                       # -> dist/Buildoffice-0.2.0-preview-windows-x64.zip and ...-setup.exe
scripts/build-release.sh --no-installer        # the zip only (no Inno Setup needed)
```

`build-release.sh` needs Inno Setup 6 to build the setup program, and it stops if it cannot find it. Use
`--no-installer` if you do not have Inno Setup and only need the zip.

The script checks the files for private information (user names, folder paths, key blocks) before it
builds the zip and the setup program, and it stops if it finds any. A zip or setup program built this way is
not guaranteed to be byte-for-byte identical to the published one.

### Security

Please report vulnerabilities through GitHub's private vulnerability reporting, not in a public issue.
See `SECURITY.md`.

### License

Buildoffice is free software, licensed under the **GNU Affero General Public License v3.0 only
(AGPL-3.0-only)**. Copyright (C) 2026 The Buildoffice Authors. See `LICENSE` and `NOTICE`.
Third-party components and their licenses are listed in `THIRD_PARTY_LICENSES`; the licenses of the
Flutter engine and Dart packages bundled with the app are in the app's `data/flutter_assets/NOTICES.Z`
(compressed with gzip).

---

## 日本語

### Buildoffice とは

Buildoffice は、ローカル主体のオープンソースのワークスペースです。ファイルは、自分の PC の
**保管庫**（アプリの専用フォルダ）に置きます。目標は、保管庫を自分の端末どうしで同期し、ドキュメント・
表計算・スライドをリアルタイムで共同編集できるようにすることです。すべての操作は、
[MCP（Model Context Protocol）](https://modelcontextprotocol.io/) で AI からも使えるようにします。

このプレビュー版は、土台にあたる **Phase 0** です。

- 保管庫を管理し、ファイルの変更を見つける Rust のコア
- AI から保管庫のファイルを読み書きするための MCP サーバー（`vault-mcp.exe`）
- 保管庫の中を見る小さなアプリ（`vault_app.exe`）

アプリの画面と、ソースコードのコメントは、今は日本語だけです。`vault-mcp.exe` のメッセージ（`--help`・
`init` の表示・MCP で AI に返すエラーの文）も、日本語だけです。

### このプレビュー版でできること

- **すぐ使える保管庫。** アプリは起動のたびに**既定の保管庫**を開きます。場所はホームのフォルダの中の
  `Buildoffice`（`%USERPROFILE%\Buildoffice`）で、なければ新しい空の保管庫として作ります。下の「既定の保管庫」を
  見てください。
- **ほかの保管庫を作る・開く**（空のフォルダか、まだないフォルダを保管庫にできます）。
- **AI から MCP で読み書きする。** `vault-mcp.exe` は stdio の MCP サーバーで、ツールは10個です：
  `vault_info`・`vault_list`・`vault_read`・`vault_write`・`vault_mkdir`・`vault_move`・`vault_delete`・
  `vault_trash_list`・`vault_restore`・`vault_changes`。`--read-only` を付けると、読むだけにできます。
- **アプリでファイルを見る。** ファイルやフォルダを作ったり、ゴミ箱へ移したりできます。
- **ゴミ箱から戻す。** 消したもの・上書きしたものは保管庫のゴミ箱に残り、アプリか `vault_restore` で戻せます。
- **ほかのアプリでの変更を知る。** `vault_changes`（とアプリの「更新」）で、外で作られた・変えられた・消された・
  名前が変わったファイルが分かります。

### まだできないこと

端末の間の同期、リアルタイムの共同編集、文書・表計算・スライドの編集、メールとカレンダー、
mac・Linux・スマートフォン版は、このプレビュー版にはありません。

今後の予定：

1. **Phase 1：** 自分の端末どうしの同期（端末の間を暗号化。自分で運用できるサーバー。メッシュ VPN）
2. **Phase 2：** ドキュメントとリアルタイムの共同編集（AI も共同編集に参加）
3. **Phase 3：** 表計算（数式のエンジン）
4. **Phase 4：** スライド、メールとカレンダー

### Windows での使い方

必要なもの：

- Windows 10 か 11（x64）。

#### セットアップのプログラムで入れる（おすすめ）

1. **ダウンロードする。** GitHub の Releases から `Buildoffice-0.2.0-preview-windows-x64-setup.exe` を取ります。
   SHA-256 は、Releases のページの値と比べられます：
   `certutil -hashfile Buildoffice-0.2.0-preview-windows-x64-setup.exe SHA256`。
   SHA-256 が合っていて分かるのは、ファイルが壊れずに届いたことまでです。だれが作ったファイルかは分かりません。
2. **開く。** セットアップのプログラムは、いまの Windows のユーザーだけに入れるので、管理者の確認は出ません。
   入る場所は `%LOCALAPPDATA%\Programs\Buildoffice` で（場所を選ぶ画面はありません）、スタートメニューに
   「Buildoffice」ができます。デスクトップのショートカットは任意で、既定では作りません。画面は、Windows の
   表示の言語に合わせて、英語か日本語で出ます。`PATH` は変えず、AI のクライアントへの登録もしません。
3. **スタートメニューから Buildoffice を起動する。** 既定の保管庫 `%USERPROFILE%\Buildoffice` が開きます。
   初めての起動では、この保管庫を作り、「保管庫を作った（ホームのフォルダの中の Buildoffice）」と出ます。
   ほかの保管庫を作るときは、右上の「×」で保管庫を閉じて、「新しく作る」のカードを使います。
   入力のしかたと、断られる場所は、下の「zip で使う」の 2 に書いてあります。
4. **AI のクライアントに MCP サーバーを登録する。** Claude Code なら、コマンドプロンプトで（既定の保管庫を
   登録する例）：

   ```bat
   claude mcp add buildoffice -- "%LOCALAPPDATA%\Programs\Buildoffice\vault-mcp.exe" --vault "%USERPROFILE%\Buildoffice" --read-only
   ```

   PowerShell では `%LOCALAPPDATA%`・`%USERPROFILE%` の代わりに `$env:LOCALAPPDATA`・`$env:USERPROFILE` と書きます
   （たとえば `--vault "$env:USERPROFILE\Buildoffice"`）。ほかの保管庫なら、その絶対パスを `--vault` に渡します。
   既定の保管庫を登録する前に、アプリを一度起動して、保管庫を作っておきます。AI に書かせたいときは
   `--read-only` を外します。登録の範囲は `local`（既定）か `user` にします。`project` は使いません
   （ユーザー名と保管庫の絶対パスが、共有される `.mcp.json` に書かれるため）。ほかの MCP のクライアントでは、
   stdio のサーバーとして、コマンド `vault-mcp.exe`（上のようにフルパスで）、引数
   `--vault <保管庫の絶対パス>` を登録します。

**更新するとき。** 先にアプリと AI のクライアントを閉じてから（AI のクライアントが `vault-mcp.exe` を動かして
いることがあります）、新しいセットアップのプログラムを開きます。古い版の上に入れ直され、保管庫には触れません。
まだ動いているプログラムがあると、セットアップのプログラムが、閉じてよいかを聞きます。

**アンインストールするとき。** 先に、AI のクライアントから登録を外します：`claude mcp remove buildoffice`。
そのあと「設定」→「アプリ」→「Buildoffice」→「アンインストール」を選びます。消えるのは、セットアップの
プログラムが入れたファイルだけです。保管庫（既定の保管庫 `%USERPROFILE%\Buildoffice` も）と、その中のファイルは
**消えません**。入れたフォルダにあなたがあとから置いたファイルも残ります。

#### zip で使う（入れずに使う）

zip には、同じプログラムが同じ並びで入っています。好きな場所に展開して、そこから動かします。

1. **ダウンロードして展開する。** GitHub の Releases から `Buildoffice-0.2.0-preview-windows-x64.zip` を
   取り、たとえば `C:\Tools` に展開します。zip の中は1つのフォルダで包まれているので、
   `C:\Tools\Buildoffice-0.2.0-preview-windows-x64\` ができます（下の例はこの場所で書きます）。
   zip の SHA-256 は、Releases のページの値と比べられます：
   `certutil -hashfile Buildoffice-0.2.0-preview-windows-x64.zip SHA256`。
   SHA-256 が合っていて分かるのは、ファイルが壊れずに届いたことまでです。だれが作ったファイルかは分かりません。
2. **アプリを起動する。** `C:\Tools\Buildoffice-0.2.0-preview-windows-x64\app\vault_app.exe` を開きます。入れた
   アプリと同じく、既定の保管庫 `%USERPROFILE%\Buildoffice` が開きます（初めてなら作ります）。

   **ほかの保管庫を作るとき**は、右上の「×」で保管庫を閉じます。画面には3つのカードがあります（いちばん上が
   既定の保管庫、次が「保管庫を開く」、下が「新しく作る」）。下のカード「新しく作る」で：
   - **置き場所**：保管庫を置くフォルダを「選ぶ…」で選ぶか、絶対パスを入れます（たとえば `D:\Vaults`）。
   - **保管庫の名前**：保管庫の名前を入れます（たとえば `MyVault`）。
   - **「作って開く」** を押します。置き場所の中に、その名前の新しいフォルダ（ここでは `D:\Vaults\MyVault`）が
     でき、そのまま開きます。

   置き場所は、ドライブ文字から始まる絶対パスです。保管庫のフォルダは、まだないか、空でなければなりません。
   次の場所は、わざと断ります：ネットワークの場所（`\\server\...`）、ドライブの根、ホームのフォルダそのもの
   （とその上のフォルダ）、AppData の下・Windows のフォルダの下・Program Files・Program Files (x86)・ProgramData の下、
   PowerShell のプロファイルのフォルダ（`Documents\PowerShell`・`Documents\WindowsPowerShell`）、ほかの保管庫の中。
   ふつうの一時フォルダは AppData の下にあるので断られます。ほかの場所の一時フォルダ（たとえば `C:\Temp`）は、
   この決まりでは断られません。名前には `\ / : * ? " < > |` と目に見えない文字を使えず、`~` のあとに数字を
   続けられず（`file~1` など）、末尾を `.` か空白にできず、`CON` や `.vault` のような予約された名前にもできません。

   その保管庫を次に開くときは、既定の保管庫を閉じてから、カード「保管庫を開く」で、保管庫のフォルダを「選ぶ…」で
   選ぶか絶対パスを入れて、「開く」を押します。「読み取り専用で開く」も選べます。アプリはパスを覚えません（起動では、
   いつも既定の保管庫を開きます）。ゴミ箱は右上にあります。

   アプリの代わりに、コマンドプロンプトでも作れます：

   ```bat
   C:\Tools\Buildoffice-0.2.0-preview-windows-x64\vault-mcp.exe init D:\Vaults\MyVault
   ```
3. **AI のクライアントに MCP サーバーを登録する。** Claude Code なら、コマンドプロンプトで（既定の保管庫を
   登録する例。ほかの保管庫なら、その絶対パスを `--vault` に渡します）：

   ```bat
   claude mcp add buildoffice -- C:\Tools\Buildoffice-0.2.0-preview-windows-x64\vault-mcp.exe --vault "%USERPROFILE%\Buildoffice" --read-only
   ```

   PowerShell では `"%USERPROFILE%\Buildoffice"` の代わりに `"$env:USERPROFILE\Buildoffice"` と書きます。
   AI に書かせたいときは `--read-only` を外します。登録の範囲は `local`（既定）か `user` にします。
   `project` は使いません（保管庫の絶対パス、つまりユーザー名を含むことがある場所が、共有される
   `.mcp.json` に書かれるため）。ほかの MCP のクライアントでは、stdio のサーバーとして、コマンド
   `vault-mcp.exe`、引数 `--vault <保管庫の絶対パス>` を登録します。

### 既定の保管庫

- **場所：** ホームのフォルダの直下の `Buildoffice`（`%USERPROFILE%\Buildoffice`）。
- **いつ：** アプリは起動のたびに、この保管庫を読み書きできる状態で開きます。フォルダがなければ、新しい空の保管庫を
  作ります。前に使った保管庫は覚えません。保管庫を閉じて戻ったときは、自動では開き直しません（いちばん上のカードの
  「既定の保管庫を開く」で開きます）。
- **使わないとき：** `Buildoffice` が保管庫でないフォルダ（空でも中身があっても）・ファイル・リンク（ジャンクション・
  シンボリックリンク。指す先が保管庫でも）のときは、何も作らず、開きません。いちばん上のカード「既定の保管庫」に
  理由が出ます。名前を変えるか、ほかへ移してから「既定の保管庫を開く」を押します。
- **消したとき：** 次の起動で、空の保管庫を作り直します。消したフォルダをごみ箱から戻すときは、先にアプリを閉じ、
  アプリが作った新しい空の `Buildoffice` をほかへ除けてから戻します。
- **アンインストール**しても、既定の保管庫は消えません。
- **0.1.0 から上げた人へ：** 上げたあとは、起動で既定の保管庫が開きます（初めての起動で作ります）。今までの保管庫は
  そのままです。「保管庫を開く」のカードで開きます。

### SmartScreen の警告について

プレビュー版のプログラムには**コード署名をしていません**。そのため、セットアップのプログラムを開いたり、
プログラムを起動したりすると、Windows の SmartScreen が「Windows によって PC が保護されました」と
出すことがあります。
このプロジェクトの GitHub の Releases から取ったセットアップのプログラムか zip で、SHA-256 が一致して
いれば、**詳細情報 → 実行** を選びます。不安なときは、下の手順でソースからビルドしてください。

### 安全のための注意

- **これはプレビュー版です。** テストはしていますが、不具合が残っているかもしれません。大事なデータは
  バックアップを取り、大事なものの唯一の写しを保管庫に置かないでください。
- **保管庫の外には触らない作りです。** パスはどれも保管庫のフォルダと照らし、リンク（ジャンクション・
  シンボリックリンク）をたどって保管庫の外へ出ません。消す・上書きするときは、前の中身を消さずに保管庫の
  ゴミ箱へ移します。MCP のサーバーは保管庫を作れません（作れるのは、あなたが `vault-mcp.exe init` か
  アプリで作るときだけです。アプリは、起動したときに既定の保管庫 `%USERPROFILE%\Buildoffice` がなければ作ります）。
- **この守りが効くのは、Buildoffice の MCP ツールを通る操作だけです。** AI のクライアント自身のシェルや
  ファイルの道具は、守りの外にあります。クライアントの権限の設定で絞ってください。
- 同じ Windows のユーザーで動くほかのプログラムは、保管庫のファイルを直接変えられます。これは Buildoffice
  では防げません。
- 保管庫の中の `.vault` フォルダは、手で変えないでください（履歴・索引・ゴミ箱が入っています）。
- `--read-only` を付けると、書くツールは AI に見えません。ただし `vault_changes` は、`.vault` の中の
  履歴と索引を更新します。

### ソースからのビルド

必要なもの：

- Windows 10/11（x64）と Git for Windows（スクリプトは **Git Bash** で動かします）
- Rust の stable（MSVC のツールチェーン）、1.89 以上（1.100.0 で確かめました）
- Visual Studio 2022 Build Tools の「C++ によるデスクトップ開発」
- Flutter 3.38（Dart 3.10）
- Python 3（release のスクリプトが、zip を作る前に行う検査にだけ使います）
- Inno Setup 6（<https://jrsoftware.org/>）。セットアップのプログラムを作るときだけ要ります（下を見てください）

Git Bash で、リポジトリの根で：

```sh
cargo test --workspace                         # Rust のコアのテスト
cargo build --release -p vault-mcp -p vault-ffi
cd app
flutter test
flutter build windows --release                # ../target/release/vault_ffi.dll を exe の隣へ写します
```

テストは、ホームのフォルダに何も作りません。ビルドしたアプリを起動すると（`flutter run` も）、公開したアプリと同じく、
既定の保管庫 `%USERPROFILE%\Buildoffice` を開きます（なければ作ります）。

公開したときと同じ手順で zip とセットアップのプログラムを作るには：

```sh
scripts/build-release.sh                       # -> dist/Buildoffice-0.2.0-preview-windows-x64.zip と ...-setup.exe
scripts/build-release.sh --no-installer        # zip だけ（Inno Setup は要りません）
```

`build-release.sh` がセットアップのプログラムを作るには Inno Setup 6 が要り、見つからないと止まります。
Inno Setup がなく、zip だけでよいときは `--no-installer` を付けます。

このスクリプトは、zip とセットアップのプログラムを作る前に、ファイルに個人の情報（ユーザー名・フォルダの場所・
鍵のかたまり）がないかを調べ、見つかれば止まります。この手順で作った zip やセットアップのプログラムが、
公開したものとバイト単位で同じになることは保証しません。

### セキュリティ

脆弱性は、公開の issue ではなく、GitHub の Private vulnerability reporting で知らせてください。
`SECURITY.md` を見てください。

### ライセンス

Buildoffice はフリーソフトウェアで、**GNU Affero General Public License v3.0 only（AGPL-3.0-only）**で
配布します。Copyright (C) 2026 The Buildoffice Authors。`LICENSE` と `NOTICE` を見てください。
使っている他のソフトウェアとそのライセンスは `THIRD_PARTY_LICENSES` にあります。アプリに入っている
Flutter のエンジンと Dart のパッケージのライセンスは、アプリの `data/flutter_assets/NOTICES.Z`
（gzip で縮めたもの）にあります。
