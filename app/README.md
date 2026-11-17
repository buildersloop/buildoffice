# vault_app（保管庫のファイルブラウザ。Phase 0・Windows）

保管庫を開いて、ファイルを見て・作って・消して（ゴミ箱へ）・戻すアプリです。表示と入力だけを受け持ち、
ロジックは Rust のコア（`crates/vault-ffi` → `vault-core`）にあります。

## ビルドと起動

リポジトリの根で：

```sh
cargo build --release -p vault-ffi      # vault_ffi.dll を作る（アプリのビルドより先に）
cd app
flutter run -d windows                  # 開発のとき
flutter build windows                   # build/windows/x64/runner/Release/ に exe と vault_ffi.dll ができる
```

`app/windows/CMakeLists.txt` が、`target/release/vault_ffi.dll` を exe の隣へ写します（なければビルドが止まります）。

## テスト

```sh
flutter analyze
VAULT_FFI_REQUIRED=1 flutter test   # release の vault_ffi.dll を呼ぶ結合テストを含む
```

`VAULT_FFI_REQUIRED=1` を付けると、DLL がないときに結合テストを飛ばさずに失敗にします（確かめや CI では、これを付けて流します）。
付けなければ、DLL がないときは、その3つを理由付きで飛ばします。

## 使い方

- 保管庫のフォルダの絶対パス（ドライブ文字から始まる形。例：`C:\Vaults\MyVault`）を入れて「開く」。
  はじめてなら「作って開く」。パスは覚えないので、開くたびに入力します。
- 消したものはゴミ箱（右上）から戻せます。
- アプリが前面にあるあいだ、ほかのアプリでの変更を調べて一覧を新しくします（右上の「更新」でも調べます）。
  間隔は 5 秒を基本に、前の調べにかかった時間に合わせて延ばします。
- 読み取り専用で開いても、変更の検知（rescan）は、保管庫の `.vault` の索引と履歴を更新します（vault-core の仕様）。
  作る・消すのボタンは出ず、ゴミ箱の「戻す」は押せません。
