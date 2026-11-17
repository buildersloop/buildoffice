# Buildoffice

**Preview release 0.1.0 (Phase 0) — Windows x64**

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

- **Create and open a vault** (an empty folder, or a folder that does not exist yet).
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
macOS/Linux/mobile builds, and an installer are **not** part of this preview.

Planned stages:

1. **Phase 1:** sync between your own devices (end-to-end encrypted; self-hostable server; mesh VPN).
2. **Phase 2:** documents and real-time co-editing, with AI as a co-editing participant.
3. **Phase 3:** spreadsheets (formula engine).
4. **Phase 4:** slides, mail and calendar.

### Using the preview on Windows

Requirements:

- Windows 10 or 11, x64.
- The Microsoft Visual C++ Redistributable (x64). Many PCs already have it. If the programs do not start
  (for example with a message about a missing `VCRUNTIME140.dll`), install the official package from
  Microsoft: <https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist>.

1. **Download and extract** `Buildoffice-0.1.0-preview-windows-x64.zip` from the GitHub Releases page,
   for example to `C:\Tools`. The zip contains one folder, so this creates
   `C:\Tools\Buildoffice-0.1.0-preview-windows-x64\` (the examples below use this path).
   You can compare the zip's SHA-256 with the value on the release page:
   `certutil -hashfile Buildoffice-0.1.0-preview-windows-x64.zip SHA256`.
   A matching SHA-256 only shows that the file was downloaded without damage; it does not show who
   made the file.
2. **Create a vault.** In Command Prompt:

   ```bat
   C:\Tools\Buildoffice-0.1.0-preview-windows-x64\vault-mcp.exe init D:\Vaults\MyVault
   ```

   Use an absolute path that starts with a drive letter. These are refused on purpose: network locations
   (`\\server\...`), the root of a drive, your home folder itself (and the folders above it), anything
   under AppData, under the Windows folder, under Program Files, Program Files (x86) or ProgramData, the
   PowerShell profile folders (`Documents\PowerShell`, `Documents\WindowsPowerShell`), and folders inside
   another vault. The usual temporary folder is under AppData, so it is refused; a temporary folder
   somewhere else (for example `C:\Temp`) is not refused by this rule. You can also create a vault from the
   app ("作って開く" = create and open).
3. **Register the MCP server with your AI client.** For Claude Code:

   ```bat
   claude mcp add buildoffice -- C:\Tools\Buildoffice-0.1.0-preview-windows-x64\vault-mcp.exe --vault D:\Vaults\MyVault --read-only
   ```

   Remove `--read-only` when you want the AI to be able to write. Register with the `local` (default) or
   `user` scope; do not use the `project` scope, because it would put your vault's absolute path (which may
   contain your user name) into a shared `.mcp.json`. Other MCP clients: register a stdio server with
   command `vault-mcp.exe` and arguments `--vault <absolute path of the vault>`.
4. **Start the app:** run `C:\Tools\Buildoffice-0.1.0-preview-windows-x64\app\vault_app.exe`.
   Enter the vault's absolute path and choose "開く" (open) or
   "作って開く" (create and open). "読み取り専用で開く" opens it read-only. The trash is at the top right.

### Windows SmartScreen warning

The preview binaries are **not code-signed**, so Windows SmartScreen may show "Windows protected your PC"
when you start `vault_app.exe` or `vault-mcp.exe`. If you downloaded the zip from this project's GitHub
Releases page and its SHA-256 matches, choose **More info → Run anyway**. If you are unsure, build from
source instead (below).

### Safety notes

- **This is a preview.** It has been tested, but it may still have bugs. Keep backups of important data,
  and do not keep the only copy of anything important in a vault yet.
- **It is designed not to touch anything outside the vault.** Every path is checked against the vault
  folder; links (junctions and symbolic links) are not followed out of the vault; deleting and overwriting
  move the old contents to the vault's trash instead of erasing them; and the MCP server cannot create a
  vault anywhere — only you can, with `vault-mcp.exe init` or the app.
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

In Git Bash, at the root of the repository:

```sh
cargo test --workspace                         # tests of the Rust core
cargo build --release -p vault-mcp -p vault-ffi
cd app
flutter test
flutter build windows --release                # copies ../target/release/vault_ffi.dll next to the exe
```

To build the release zip with the same steps as the published one:

```sh
scripts/build-release.sh                       # -> dist/Buildoffice-0.1.0-preview-windows-x64.zip
```

The script checks the files for private information (user names, folder paths, key blocks) before it
builds the zip, and it stops if it finds any. A zip built this way is not guaranteed to be byte-for-byte
identical to the published one.

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

- **保管庫を作る・開く**（空のフォルダか、まだないフォルダを保管庫にできます）。
- **AI から MCP で読み書きする。** `vault-mcp.exe` は stdio の MCP サーバーで、ツールは10個です：
  `vault_info`・`vault_list`・`vault_read`・`vault_write`・`vault_mkdir`・`vault_move`・`vault_delete`・
  `vault_trash_list`・`vault_restore`・`vault_changes`。`--read-only` を付けると、読むだけにできます。
- **アプリでファイルを見る。** ファイルやフォルダを作ったり、ゴミ箱へ移したりできます。
- **ゴミ箱から戻す。** 消したもの・上書きしたものは保管庫のゴミ箱に残り、アプリか `vault_restore` で戻せます。
- **ほかのアプリでの変更を知る。** `vault_changes`（とアプリの「更新」）で、外で作られた・変えられた・消された・
  名前が変わったファイルが分かります。

### まだできないこと

端末の間の同期、リアルタイムの共同編集、文書・表計算・スライドの編集、メールとカレンダー、
mac・Linux・スマートフォン版、インストーラーは、このプレビュー版にはありません。

今後の予定：

1. **Phase 1：** 自分の端末どうしの同期（端末の間を暗号化。自分で運用できるサーバー。メッシュ VPN）
2. **Phase 2：** ドキュメントとリアルタイムの共同編集（AI も共同編集に参加）
3. **Phase 3：** 表計算（数式のエンジン）
4. **Phase 4：** スライド、メールとカレンダー

### Windows での使い方

必要なもの：

- Windows 10 か 11（x64）。
- Microsoft Visual C++ 再頒布可能パッケージ（x64）。入っている PC も多いです。プログラムが起動しないとき
  （たとえば `VCRUNTIME140.dll` がないという表示が出るとき）は、Microsoft の公式のパッケージを入れてください：
  <https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist>

1. **ダウンロードして展開する。** GitHub の Releases から `Buildoffice-0.1.0-preview-windows-x64.zip` を
   取り、たとえば `C:\Tools` に展開します。zip の中は1つのフォルダで包まれているので、
   `C:\Tools\Buildoffice-0.1.0-preview-windows-x64\` ができます（下の例はこの場所で書きます）。
   zip の SHA-256 は、Releases のページの値と比べられます：
   `certutil -hashfile Buildoffice-0.1.0-preview-windows-x64.zip SHA256`。
   SHA-256 が合っていて分かるのは、ファイルが壊れずに届いたことまでです。だれが作ったファイルかは分かりません。
2. **保管庫を作る。** コマンドプロンプトで：

   ```bat
   C:\Tools\Buildoffice-0.1.0-preview-windows-x64\vault-mcp.exe init D:\Vaults\MyVault
   ```

   ドライブ文字から始まる絶対パスで書きます。次の場所は、わざと断ります：ネットワークの場所（`\\server\...`）、
   ドライブの根、ホームのフォルダそのもの（とその上のフォルダ）、AppData の下・Windows のフォルダの下・
   Program Files・Program Files (x86)・ProgramData の下、PowerShell のプロファイルのフォルダ
   （`Documents\PowerShell`・`Documents\WindowsPowerShell`）、ほかの保管庫の中。ふつうの一時フォルダは
   AppData の下にあるので断られます。ほかの場所の一時フォルダ（たとえば `C:\Temp`）は、この決まりでは断られません。
   アプリの「作って開く」でも作れます。
3. **AI のクライアントに MCP サーバーを登録する。** Claude Code なら：

   ```bat
   claude mcp add buildoffice -- C:\Tools\Buildoffice-0.1.0-preview-windows-x64\vault-mcp.exe --vault D:\Vaults\MyVault --read-only
   ```

   AI に書かせたいときは `--read-only` を外します。登録の範囲は `local`（既定）か `user` にします。
   `project` は使いません（保管庫の絶対パス、つまりユーザー名を含むことがある場所が、共有される
   `.mcp.json` に書かれるため）。ほかの MCP のクライアントでは、stdio のサーバーとして、コマンド
   `vault-mcp.exe`、引数 `--vault <保管庫の絶対パス>` を登録します。
4. **アプリを起動する。** `C:\Tools\Buildoffice-0.1.0-preview-windows-x64\app\vault_app.exe` を開き、保管庫の絶対パスを入れて「開く」か「作って開く」を
   選びます。「読み取り専用で開く」も選べます。ゴミ箱は右上にあります。

### SmartScreen の警告について

プレビュー版のプログラムには**コード署名をしていません**。そのため、`vault_app.exe` や `vault-mcp.exe` を
起動すると、Windows の SmartScreen が「Windows によって PC が保護されました」と出すことがあります。
このプロジェクトの GitHub の Releases から取った zip で、SHA-256 が一致していれば、**詳細情報 → 実行** を
選びます。不安なときは、下の手順でソースからビルドしてください。

### 安全のための注意

- **これはプレビュー版です。** テストはしていますが、不具合が残っているかもしれません。大事なデータは
  バックアップを取り、大事なものの唯一の写しを保管庫に置かないでください。
- **保管庫の外には触らない作りです。** パスはどれも保管庫のフォルダと照らし、リンク（ジャンクション・
  シンボリックリンク）をたどって保管庫の外へ出ません。消す・上書きするときは、前の中身を消さずに保管庫の
  ゴミ箱へ移します。MCP のサーバーは保管庫を作れません（作れるのは、あなたが `vault-mcp.exe init` か
  アプリで作るときだけです）。
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

Git Bash で、リポジトリの根で：

```sh
cargo test --workspace                         # Rust のコアのテスト
cargo build --release -p vault-mcp -p vault-ffi
cd app
flutter test
flutter build windows --release                # ../target/release/vault_ffi.dll を exe の隣へ写します
```

公開したときと同じ手順で zip を作るには：

```sh
scripts/build-release.sh                       # -> dist/Buildoffice-0.1.0-preview-windows-x64.zip
```

このスクリプトは、zip を作る前に、ファイルに個人の情報（ユーザー名・フォルダの場所・鍵のかたまり）が
ないかを調べ、見つかれば止まります。この手順で作った zip が、公開したものとバイト単位で同じになることは
保証しません。

### セキュリティ

脆弱性は、公開の issue ではなく、GitHub の Private vulnerability reporting で知らせてください。
`SECURITY.md` を見てください。

### ライセンス

Buildoffice はフリーソフトウェアで、**GNU Affero General Public License v3.0 only（AGPL-3.0-only）**で
配布します。Copyright (C) 2026 The Buildoffice Authors。`LICENSE` と `NOTICE` を見てください。
使っている他のソフトウェアとそのライセンスは `THIRD_PARTY_LICENSES` にあります。アプリに入っている
Flutter のエンジンと Dart のパッケージのライセンスは、アプリの `data/flutter_assets/NOTICES.Z`
（gzip で縮めたもの）にあります。
