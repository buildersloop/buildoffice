#!/usr/bin/env bash
# Buildoffice の Windows のプレビュー版（zip とインストーラーの setup.exe）を作る（Git Bash で動かす）。
#
# 0. 版：根の Cargo.toml の [workspace.package] の version と、app/pubspec.yaml の version（どちらも X.Y.Z）を読み、
#    違えば止まる。配る名前は Buildoffice-X.Y.Z-preview-windows-x64、Windows の版の情報は X.Y.Z.0。
# 1. Rust のコア：vault-mcp.exe と vault_ffi.dll を release で作る。exe・DLL に、この PC の場所（ユーザー名を
#    含むことがある）が残らないように、cargo の置き場・rustup の置き場・ソースの根を --remap-path-prefix で
#    置き換え、PDB の場所はファイル名だけにする（/PDBALTPATH）。C/C++ のランタイムは静的に組み込む
#    （-Ctarget-feature=+crt-static。別に入れるランタイムがない PC でも動くように）。テスト用の入口が入っていないことも確かめる。
# 2. 画面のアプリ：flutter build windows --release（app/windows/CMakeLists.txt が target/release/vault_ffi.dll を
#    exe の隣へ写す。C/C++ のランタイムは、同じ CMakeLists.txt で静的に組み込む＝/MT）。
# 3. dist/<名前>/ にまとめる（根に LICENSE があれば、それも入れる）。
# 4. まとめた中身を、zip にする前に検査する：
#    (a) scripts/check-runtime-deps.py：exe・DLL が、別に入れる C/C++ のランタイムの DLL（VCRUNTIME*・MSVCP*・
#        CONCRT*・VCOMP*・VCCORLIB*・MSVCR<数字>*・MFC<数字>*）と、動的な C のランタイム（api-ms-win-crt-*・
#        ucrtbase*）を読み込まないこと（いまは完全に静的。その状態を固定する）。
#    (b) scripts/check-private-info.py：ユーザー名・ホームの場所・ソースの場所・鍵のかたまり・トークンの形が 0 件で
#        あること（ASCII と UTF-16。gzip の中の data/flutter_assets/NOTICES.Z も）。scripts/forbidden-words.txt が
#        あるときは、その一覧の語（公開してはいけない語）も 0 件であること（開発用のリポジトリにだけあるファイル。
#        公開のリポジトリにはないので、そのときは語の検査を飛ばす）。
#    1件でもあれば、zip を作らずに止まる（まとめた dist/<名前>/ は、
#    「公開不可」の印を付けて dist/<名前>.rejected-<日時>/ へ移す。消さない）。
# 5. 検査に通ったときだけ、Windows の tar.exe（-a で拡張子から zip にする）で zip にして、SHA-256 を書く。
# 6. インストーラー（Inno Setup 6 の ISCC.exe で installer/buildoffice.iss から作る。中身は 3 でまとめたもの）：
#    (a) 確かめ用に、縮めない setup を一時の場所（dist/.setup-check-*/）に作り、check-private-info.py で検査する
#        （縮めた setup.exe の中は検査で読めないので、同じ中身を縮めずに入れたもので確かめる）。通れば捨てる。
#    (b) 配る setup.exe（dist/<名前>-setup.exe）を作り、それも検査して、通れば SHA-256 を書く。
#    どちらかが落ちたら、その setup を「公開不可」の印を付けて dist/<名前>-setup.rejected-<日時>/ へ移して止まる（消さない）。
#    ISCC が途中で落ちたときなども同じで、検査していない setup は dist に残さない（上と同じ rejected のフォルダへ移す）。
#    6 のどこかで止まったときは、dist にも「公開不可」の印（dist/DO-NOT-PUBLISH.txt）を置く：zip と .zip.sha256 が
#    残っていても、公開しない（zip と setup.exe は対で公開する）。印があるうちは、次の作成も止まる（印は人が消す）。
#
# 使い方：リポジトリの根で  scripts/build-release.sh [--no-installer]
#   --no-installer  インストーラーを作らない（zip だけ。Inno Setup のない PC で使う）。
# インストーラーを作るときは、Inno Setup 6 の ISCC.exe がなければ、ビルドの前に止まる。探す順：環境変数 ISCC
# （ISCC.exe の場所）→ PATH → %LOCALAPPDATA%\Programs\Inno Setup 6 → %ProgramFiles(x86)%\Inno Setup 6。
# 見つけた ISCC の場所と版（Inno Setup の X.Y.Z）は、ログに出す。版が、THIRD_PARTY_LICENSES に書いた Inno Setup の版と
# 違うとき、読めないときも、ビルドの前に止まる（版は、ISCC が出す「Compiler engine version: Inno Setup X.Y.Z」の行から読む）。
# dist/<名前>・dist/<名前>.zip・dist/<名前>-setup.exe のどれかがすでにあるときは止まる（中のものを消さない）。
# 根に「公開不可」の印（DO-NOT-PUBLISH.txt）があるときも止まる（公開用の書き出しが検査に落ちたときの印）。
#
# 公開のリポジトリの根で流すときは、根に LICENSE（GitHub がリポジトリを作るときに置く AGPL の本文）が
# なければ止まる。開発用のリポジトリで流すときは、README.md・NOTICE を public/ の下から取り、LICENSE が
# なくても注意だけで続ける。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="$ROOT/dist"

win() { cygpath -w "$1"; }

if [ -e "$ROOT/DO-NOT-PUBLISH.txt" ]; then
  echo "公開不可の印がある：$ROOT/DO-NOT-PUBLISH.txt（書き出しの検査に落ちた。原因を直して、書き出しからやり直す）" >&2
  exit 1
fi

WITH_INSTALLER=1
for arg in "$@"; do
  case "$arg" in
    --no-installer) WITH_INSTALLER=0 ;;
    *)
      echo "知らない引数：$arg（使えるのは --no-installer だけ）" >&2
      exit 1
      ;;
  esac
done

# --- 0. 版（Cargo.toml と pubspec.yaml がそろっていること） ---
CARGO_VERSION=""
if [ -f "$ROOT/Cargo.toml" ]; then
  CARGO_VERSION="$(awk '
    /^[[:space:]]*\[/ { section = $0; gsub(/[[:space:]]/, "", section); next }
    section == "[workspace.package]" && /^[[:space:]]*version[[:space:]]*=/ {
      line = $0
      sub(/^[^"]*"/, "", line)
      sub(/".*$/, "", line)
      print line
      exit
    }' "$ROOT/Cargo.toml" | tr -d '\r')"
fi
PUB_VERSION=""
if [ -f "$ROOT/app/pubspec.yaml" ]; then
  PUB_VERSION="$(awk '/^version:/ { print $2; exit }' "$ROOT/app/pubspec.yaml" | tr -d '\r')"
fi
VERSION_RE='^[0-9]+\.[0-9]+\.[0-9]+$'
if ! [[ "$CARGO_VERSION" =~ $VERSION_RE ]]; then
  echo "版が読めない：Cargo.toml の [workspace.package] の version（X.Y.Z の形で書く）：'$CARGO_VERSION'" >&2
  exit 1
fi
if ! [[ "$PUB_VERSION" =~ $VERSION_RE ]]; then
  echo "版が読めない：app/pubspec.yaml の version（X.Y.Z の形で書く。+ のビルド番号は付けない）：'$PUB_VERSION'" >&2
  exit 1
fi
if [ "$CARGO_VERSION" != "$PUB_VERSION" ]; then
  echo "版が違う：Cargo.toml は $CARGO_VERSION、app/pubspec.yaml は $PUB_VERSION（同じにしてからやり直す）" >&2
  exit 1
fi
VERSION="$CARGO_VERSION-preview"
WIN_VERSION="$CARGO_VERSION.0"
NAME="Buildoffice-$VERSION-windows-x64"
SETUP_NAME="$NAME-setup"
STAGE="$DIST/$NAME"
ZIP="$DIST/$NAME.zip"
SETUP="$DIST/$SETUP_NAME.exe"

if [ -e "$DIST/DO-NOT-PUBLISH.txt" ]; then
  echo "公開不可の印がある：$DIST/DO-NOT-PUBLISH.txt（前の作成が、インストーラーの作成か検査で止まった。dist の zip なども公開しない。" >&2
  echo "中を確かめて、dist の中のものを片づけてから、印を消してやり直す。消すのは人が行う）" >&2
  exit 1
fi
for p in "$STAGE" "$ZIP" "$SETUP"; do
  if [ -e "$p" ]; then
    echo "すでにある：$p（中を確かめて、消してからやり直す）" >&2
    exit 1
  fi
done

# --- インストーラーを作る道具（Inno Setup 6 の ISCC.exe）。なければ、ビルドの前に止まる ---
find_iscc() {
  local c d candidates=()
  if [ -n "${ISCC:-}" ]; then
    c="$(cygpath -u "$ISCC")"
    if [ -f "$c" ]; then
      echo "$c"
      return 0
    fi
    echo "環境変数 ISCC の場所に ISCC.exe がない：$ISCC" >&2
    return 1
  fi
  c="$(command -v iscc 2>/dev/null || true)"
  if [ -n "$c" ]; then
    echo "$c"
    return 0
  fi
  # ユーザーごとに入れた場所と、PC 全体に入れた場所（ProgramFiles(x86) は、名前にかっこがあるので printenv で読む。
  # ほかのプログラムから起動されると名前が大文字になっていることがあるので、両方の書き方で読む）
  d="$(printenv LOCALAPPDATA || true)"
  [ -z "$d" ] || candidates+=("$(cygpath -u "$d")/Programs/Inno Setup 6/ISCC.exe")
  d="$(printenv 'ProgramFiles(x86)' || printenv 'PROGRAMFILES(X86)' || true)"
  [ -z "$d" ] || candidates+=("$(cygpath -u "$d")/Inno Setup 6/ISCC.exe")
  for c in ${candidates[@]+"${candidates[@]}"}; do
    if [ -f "$c" ]; then
      echo "$c"
      return 0
    fi
  done
  return 1
}
ISCC_EXE=""
if [ "$WITH_INSTALLER" = 1 ]; then
  if ! ISCC_EXE="$(find_iscc)"; then
    echo "Inno Setup 6 の ISCC.exe が見つからない（インストーラーを作るのに使う）。Inno Setup 6 を入れるか、" >&2
    echo "環境変数 ISCC に ISCC.exe の場所を入れる。zip だけを作るときは --no-installer を付ける" >&2
    exit 1
  fi
  if [ ! -f "$ROOT/installer/buildoffice.iss" ]; then
    echo "インストーラーの設定がない：$ROOT/installer/buildoffice.iss" >&2
    exit 1
  fi
  if [ ! -f "$ROOT/THIRD_PARTY_LICENSES" ]; then
    echo "THIRD_PARTY_LICENSES がない：$ROOT/THIRD_PARTY_LICENSES（Inno Setup の版を、ここに書いた版と照らす）" >&2
    exit 1
  fi
  # 使う ISCC の版は、THIRD_PARTY_LICENSES に書いた Inno Setup の版（配る setup.exe に入る Inno のプログラムの版）と
  # 同じでなければ、ビルドの前に止まる。ISCC は、コンパイルのたびに「Compiler engine version: Inno Setup X.Y.Z」の行を
  # 出す。空の入力を渡すと、その行を出してから「設定がない」と失敗する（何も作らない・書かない）ので、その行を読む。
  # 読めないときは、場所と読めた出力を出して止まる（major の 6 だけで通す形にはしない）。
  INNO_LISTED="$(grep -a -o -E 'Inno Setup [0-9]+\.[0-9]+\.[0-9]+' "$ROOT/THIRD_PARTY_LICENSES" | sed 's/^Inno Setup //' | sort -u | tr -d '\r' || true)"
  if [ "$(printf '%s\n' "$INNO_LISTED" | grep -c .)" != 1 ]; then
    echo "THIRD_PARTY_LICENSES に書いた Inno Setup の版が、1つに決まらない（「Inno Setup X.Y.Z」の形で、同じ版だけを書く）：$(printf '%s' "$INNO_LISTED" | tr '\n' ' ')" >&2
    exit 1
  fi
  ISCC_PROBE="$(printf '' | MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' "$ISCC_EXE" /O- - 2>&1 | tr -d '\r' || true)"
  ISCC_VERSION="$(printf '%s\n' "$ISCC_PROBE" | sed -n 's/^Compiler engine version: Inno Setup \([0-9][0-9.]*\)$/\1/p' | head -1 || true)"
  echo "インストーラーを作る道具：$(win "$ISCC_EXE")（Inno Setup ${ISCC_VERSION:-版が読めない}。THIRD_PARTY_LICENSES は $INNO_LISTED）"
  if [ -z "$ISCC_VERSION" ]; then
    echo "ISCC の版が読めない：$ISCC_EXE（読めた出力の先頭）：" >&2
    printf '%s\n' "$ISCC_PROBE" | head -n 5 >&2
    exit 1
  fi
  if [ "$ISCC_VERSION" != "$INNO_LISTED" ]; then
    echo "ISCC の版が違う：$ISCC_EXE は Inno Setup $ISCC_VERSION、THIRD_PARTY_LICENSES は $INNO_LISTED" >&2
    echo "（同じ版の Inno Setup を使うか、版を上げて THIRD_PARTY_LICENSES の版・license.txt を書き直してからやり直す）" >&2
    exit 1
  fi
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
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$(win "$CARGO_HOME_DIR")=/cargo${SEP}--remap-path-prefix=$(win "$RUSTUP_HOME_DIR")=/rustup${SEP}--remap-path-prefix=$(win "$ROOT")=/buildoffice${SEP}-Clink-arg=/PDBALTPATH:%_PDB%${SEP}-Ctarget-feature=+crt-static"
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

# --- 4. zip にする前の検査（C/C++ のランタイム。個人の情報・鍵のかたまり・公開してはいけない語。gzip の中も） ---
FORBIDDEN_ARGS=()
if [ -f scripts/forbidden-words.txt ]; then
  FORBIDDEN_ARGS=(--forbidden-file scripts/forbidden-words.txt)
else
  echo "注意：scripts/forbidden-words.txt がない（公開してはいけない語の検査は飛ばす。個人の情報・鍵の検査は行う）" >&2
fi
check_private() {
  "$PY" scripts/check-private-info.py ${FORBIDDEN_ARGS[@]+"${FORBIDDEN_ARGS[@]}"} \
    --needle "$(win "$ROOT")" --needle "$ROOT" \
    --needle "$(win "$CARGO_HOME_DIR")" --needle "$(win "$RUSTUP_HOME_DIR")" \
    "$@"
}
reject_stage() {
  local rejected
  rejected="$DIST/$NAME.rejected-$(date +%Y%m%d-%H%M%S)"
  mv "$STAGE" "$rejected"
  printf '%s\n' "検査に落ちた：公開してはいけない。zip は作っていない。" >"$rejected/DO-NOT-PUBLISH.txt"
  echo "検査に落ちた（$1）。zip は作らない。まとめたものは $rejected に移した（消していない）" >&2
  exit 1
}
"$PY" scripts/check-runtime-deps.py "$STAGE" || reject_stage "C/C++ のランタイムの DLL を読み込む exe・DLL がある"
check_private "$STAGE" || reject_stage "個人の情報・鍵・公開してはいけない語"

# --- 5. zip にして SHA-256 を書く ---
(cd "$DIST" && "$TAR" -a -c -f "$NAME.zip" "$NAME")
(cd "$DIST" && sha256sum "$NAME.zip" | tee "$NAME.zip.sha256")
echo "できた：$ZIP"

if [ "$WITH_INSTALLER" = 0 ]; then
  echo "インストーラーは作らない（--no-installer）"
  exit 0
fi

# --- 6. インストーラー（setup.exe） ---
# ここから先で止まったとき（検査に落ちた・ISCC が途中で落ちた・Ctrl-C など）の後始末（on_exit）：
#  - 検査していない setup を dist に残さない：確かめ用の縮めない setup（dist/.setup-check-*/ の中）と、配る
#    setup.exe の検査の前のものは、「公開不可」の印を付けて dist/<名前>-setup.rejected-<日時>/ へ移す（消さない）。
#  - dist に「公開不可」の印（dist/DO-NOT-PUBLISH.txt）を置く。zip と .zip.sha256 が残っていても、公開しない。
CHECK_DIR=""
SETUP_UNCHECKED=0
SETUP_REJECTED=""
# setup を、公開不可の印を付けた rejected のフォルダへ移す（消さない）。1回の実行では、同じフォルダに入れる
aside_setup() {
  if [ -z "$SETUP_REJECTED" ]; then
    SETUP_REJECTED="$DIST/$SETUP_NAME.rejected-$(date +%Y%m%d-%H%M%S)"
    mkdir -p "$SETUP_REJECTED"
    printf '%s\n' "検査に落ちたか、検査の前に作成が止まった：公開してはいけない。" >"$SETUP_REJECTED/DO-NOT-PUBLISH.txt"
  fi
  mv "$1" "$SETUP_REJECTED/"
}
on_exit() {
  local rc=$? f
  [ "$rc" -ne 0 ] || return 0
  set +e
  if [ -n "$CHECK_DIR" ] && [ -d "$CHECK_DIR" ]; then
    while IFS= read -r -d '' f; do
      aside_setup "$f"
    done < <(find "$CHECK_DIR" -mindepth 1 -maxdepth 1 -print0)
    rmdir "$CHECK_DIR"
  fi
  if [ "$SETUP_UNCHECKED" = 1 ] && [ -e "$SETUP" ]; then
    aside_setup "$SETUP"
  fi
  printf '%s\n' "公開不可：インストーラーの作成か検査で止まった。この dist の zip と .zip.sha256 も公開しない（zip と setup.exe は対で公開する）。原因を直し、dist の中のものを片づけて、この印を消してからやり直す。" >"$DIST/DO-NOT-PUBLISH.txt"
  echo "dist に公開不可の印を置いた：$DIST/DO-NOT-PUBLISH.txt（zip も公開しない。中のものは消していない）" >&2
  return 0
}
trap on_exit EXIT

# Git Bash は「/」で始まる引数を Windows のパスに直してしまうので、ISCC の呼び出しでは直させない
run_iscc() {
  local out_dir="$1" base="$2"
  shift 2
  MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' "$ISCC_EXE" /Qp \
    "/DAppVersion=$VERSION" "/DWinVersion=$WIN_VERSION" "/DStageDir=$(win "$STAGE")" \
    "$@" "/O$(win "$out_dir")" "/F$base" "$(win "$ROOT/installer/buildoffice.iss")"
  echo # ISCC の進み具合の表示は、改行で終わらない
}
reject_setup() {
  local file="$1" reason="$2"
  aside_setup "$file"
  echo "インストーラーの検査に落ちた（$reason）。$SETUP_REJECTED に移した（消していない）" >&2
  exit 1
}
# 検査は、ないファイルを「0 件」と数えるので、できたことを先に確かめる
need_file() {
  if [ ! -f "$1" ]; then
    echo "ISCC が作るはずのファイルがない：$1" >&2
    exit 1
  fi
}

# (a) 確かめ用：同じ中身を縮めずに入れた setup を作って検査し、通れば捨てる
CHECK_DIR="$(mktemp -d "$DIST/.setup-check-XXXXXX")"
run_iscc "$CHECK_DIR" "$SETUP_NAME-uncompressed" /DUncompressed=1
CHECK_SETUP="$CHECK_DIR/$SETUP_NAME-uncompressed.exe"
need_file "$CHECK_SETUP"
check_private "$CHECK_SETUP" || reject_setup "$CHECK_SETUP" "縮めない setup の中に、個人の情報・鍵・公開してはいけない語"
rm -f "$CHECK_SETUP"
rmdir "$CHECK_DIR"
CHECK_DIR=""

# (b) 配る setup.exe
SETUP_UNCHECKED=1
run_iscc "$DIST" "$SETUP_NAME"
need_file "$SETUP"
check_private "$SETUP" || reject_setup "$SETUP" "setup.exe に、個人の情報・鍵・公開してはいけない語"
SETUP_UNCHECKED=0
(cd "$DIST" && sha256sum "$SETUP_NAME.exe" | tee "$SETUP_NAME.exe.sha256")
echo "できた：$SETUP"
