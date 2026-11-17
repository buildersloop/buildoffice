#!/usr/bin/env bash
# Buildoffice の Windows のプレビュー版（zip）を作る（Git Bash で動かす）。
#
# 1. Rust のコア：vault-mcp.exe と vault_ffi.dll を release で作る。exe・DLL に、この PC の場所（ユーザー名を
#    含むことがある）が残らないように、cargo の置き場・rustup の置き場・ソースの根を --remap-path-prefix で
#    置き換え、PDB の場所はファイル名だけにする（/PDBALTPATH）。テスト用の入口が入っていないことも確かめる。
# 2. 画面のアプリ：flutter build windows --release（app/windows/CMakeLists.txt が target/release/vault_ffi.dll を
#    exe の隣へ写す）。
# 3. dist/<名前>/ にまとめる（根に LICENSE があれば、それも入れる）。
# 4. まとめた中身を、zip にする前に検査する（scripts/check-private-info.py）：ユーザー名・ホームの場所・ソースの
#    場所・鍵のかたまり・トークンの形が 0 件であること（ASCII と UTF-16。gzip の中の
#    data/flutter_assets/NOTICES.Z も）。scripts/forbidden-words.txt があるときは、その一覧の語（公開してはいけない語）も
#    0 件であること（開発用のリポジトリにだけあるファイル。公開のリポジトリにはないので、そのときは語の検査を飛ばす）。
#    1件でもあれば、zip を作らずに止まる（まとめた dist/<名前>/ は、
#    「公開不可」の印を付けて dist/<名前>.rejected-<日時>/ へ移す。消さない）。
# 5. 検査に通ったときだけ、Windows の tar.exe（-a で拡張子から zip にする）で zip にして、SHA-256 を書く。
#
# 使い方：リポジトリの根で  scripts/build-release.sh
# dist/<名前> か dist/<名前>.zip がすでにあるときは止まる（中のものを消さない）。
# 根に「公開不可」の印（DO-NOT-PUBLISH.txt）があるときも止まる（公開用の書き出しが検査に落ちたときの印）。
#
# 公開のリポジトリの根で流すときは、根に LICENSE（GitHub がリポジトリを作るときに置く AGPL の本文）が
# なければ止まる。開発用のリポジトリで流すときは、README.md・NOTICE を public/ の下から取り、LICENSE が
# なくても注意だけで続ける。

set -euo pipefail

VERSION="0.1.0-preview"
NAME="Buildoffice-$VERSION-windows-x64"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="$ROOT/dist"
STAGE="$DIST/$NAME"
ZIP="$DIST/$NAME.zip"

win() { cygpath -w "$1"; }

if [ -e "$ROOT/DO-NOT-PUBLISH.txt" ]; then
  echo "公開不可の印がある：$ROOT/DO-NOT-PUBLISH.txt（書き出しの検査に落ちた。原因を直して、書き出しからやり直す）" >&2
  exit 1
fi

if [ -e "$STAGE" ] || [ -e "$ZIP" ]; then
  echo "すでにある：$STAGE か $ZIP（中を確かめて、消してからやり直す）" >&2
  exit 1
fi

PY=""
for cand in python3 python; do
  if command -v "$cand" >/dev/null 2>&1 && "$cand" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 8) else 1)' >/dev/null 2>&1; then
    PY="$cand"
    break
  fi
done
if [ -z "$PY" ]; then
  echo "Python 3 がない（zip にする前の検査に使う）" >&2
  exit 1
fi

TAR="$(cygpath -u "${SYSTEMROOT:-C:\\Windows}")/System32/tar.exe"
if [ ! -x "$TAR" ]; then
  echo "Windows の tar.exe がない：$TAR" >&2
  exit 1
fi

cd "$ROOT"

# 公開のリポジトリでは根の README.md・NOTICE。開発用のリポジトリでは public/ の下のもの
README_SRC=README.md
NOTICE_SRC=NOTICE
DEV_REPO=0
if [ -f public/README.md ]; then
  README_SRC=public/README.md
  DEV_REPO=1
fi
[ -f public/NOTICE ] && NOTICE_SRC=public/NOTICE

if [ "$DEV_REPO" = 0 ] && [ ! -f LICENSE ]; then
  echo "LICENSE がない（公開のリポジトリの根に GitHub が置く AGPL の本文）。zip に本文が入らないので止める" >&2
  exit 1
fi

# --- 1. Rust のコア ---
CARGO_HOME_DIR="${CARGO_HOME:-$HOME/.cargo}"
RUSTUP_HOME_DIR="${RUSTUP_HOME:-$HOME/.rustup}"
SEP=$'\x1f'
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$(win "$CARGO_HOME_DIR")=/cargo${SEP}--remap-path-prefix=$(win "$RUSTUP_HOME_DIR")=/rustup${SEP}--remap-path-prefix=$(win "$ROOT")=/buildoffice${SEP}-Clink-arg=/PDBALTPATH:%_PDB%"
unset RUSTFLAGS
# app/windows/CMakeLists.txt は、根の target/release/vault_ffi.dll を写すので、出力先を変えない
unset CARGO_TARGET_DIR
cargo build --release -p vault-mcp -p vault-ffi

for pair in \
  "target/release/vault-mcp.exe:VAULT_MCP_TEST_PANIC_TOOL" \
  "target/release/vault_ffi.dll:__vault_ffi_test_panic" \
  "target/release/vault-mcp.exe:__vault_core_test_hook" \
  "target/release/vault_ffi.dll:__vault_core_test_hook"; do
  file="${pair%%:*}"
  text="${pair##*:}"
  n="$(grep -a -c -F "$text" "$file" || true)"
  if [ "$n" != 0 ]; then
    echo "テスト用の文字列が入っている：$file に $text" >&2
    exit 1
  fi
done

# --- 2. 画面のアプリ ---
(cd app && flutter pub get && flutter build windows --release)
APP_OUT="app/build/windows/x64/runner/Release"

# --- 3. まとめる ---
mkdir -p "$STAGE/app"
cp -p target/release/vault-mcp.exe "$STAGE/"
# アプリの出力を写す。デバッグの情報（PDB など）は写さない（作った PC の場所が入るため）
while IFS= read -r -d '' f; do
  mkdir -p "$STAGE/app/$(dirname "$f")"
  cp -p "$APP_OUT/$f" "$STAGE/app/$f"
done < <(cd "$APP_OUT" && find . -type f ! -name '*.pdb' ! -name '*.ilk' ! -name '*.exp' ! -name '*.lib' -print0)
cp -p "$README_SRC" "$STAGE/README.md"
cp -p "$NOTICE_SRC" "$STAGE/NOTICE"
cp -p THIRD_PARTY_LICENSES "$STAGE/"
if [ -f SECURITY.md ]; then
  cp -p SECURITY.md "$STAGE/"
elif [ -f public/SECURITY.md ]; then
  cp -p public/SECURITY.md "$STAGE/SECURITY.md"
fi
if [ -f LICENSE ]; then
  cp -p LICENSE "$STAGE/"
else
  echo "注意：LICENSE がない（公開のリポジトリを作るときに GitHub が置く）。zip に AGPL の本文が入らない" >&2
fi
for need in "$STAGE/vault-mcp.exe" "$STAGE/app/vault_app.exe" "$STAGE/app/vault_ffi.dll" \
  "$STAGE/app/flutter_windows.dll" "$STAGE/app/data/flutter_assets/NOTICES.Z"; do
  if [ ! -f "$need" ]; then
    echo "まとめたものに足りない：$need" >&2
    exit 1
  fi
done

# --- 4. zip にする前の検査（個人の情報・鍵のかたまり・公開してはいけない語。gzip の中も） ---
FORBIDDEN_ARGS=()
if [ -f scripts/forbidden-words.txt ]; then
  FORBIDDEN_ARGS=(--forbidden-file scripts/forbidden-words.txt)
else
  echo "注意：scripts/forbidden-words.txt がない（公開してはいけない語の検査は飛ばす。個人の情報・鍵の検査は行う）" >&2
fi
if ! "$PY" scripts/check-private-info.py ${FORBIDDEN_ARGS[@]+"${FORBIDDEN_ARGS[@]}"} \
  --needle "$(win "$ROOT")" --needle "$ROOT" \
  --needle "$(win "$CARGO_HOME_DIR")" --needle "$(win "$RUSTUP_HOME_DIR")" \
  "$STAGE"; then
  REJECTED="$DIST/$NAME.rejected-$(date +%Y%m%d-%H%M%S)"
  mv "$STAGE" "$REJECTED"
  printf '%s\n' "検査に落ちた：公開してはいけない。zip は作っていない。" >"$REJECTED/DO-NOT-PUBLISH.txt"
  echo "検査に落ちた。zip は作らない。まとめたものは $REJECTED に移した（消していない）" >&2
  exit 1
fi

# --- 5. zip にして SHA-256 を書く ---
(cd "$DIST" && "$TAR" -a -c -f "$NAME.zip" "$NAME")
(cd "$DIST" && sha256sum "$NAME.zip" | tee "$NAME.zip.sha256")

echo "できた：$ZIP"
