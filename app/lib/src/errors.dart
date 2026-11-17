/// vault-ffi（vault-core）が返したエラー（`{"error":{"code","message"}}`）か、Dart の側で起きた同じ形のエラー。
class VaultException implements Exception {
  const VaultException(this.code, this.message);

  /// エラーのコード（`NOT_FOUND` など。vault-core の ErrorCode）。
  final String code;

  /// vault-core の説明の文（絶対パスを含まない）。画面にそのまま出すかは [errorText] が決める。
  final String message;

  @override
  String toString() => '$code: $message';
}

/// vault-core のエラーでない例外のときの決まった文（部品を読み込めない・閉じた保管庫を使ったなど、どれにも使う）。
const String unexpectedErrorText = '内部のエラーが起きました（保管庫を開き直すか、アプリを起動し直す）';

/// コードごとの決まった文（画面に出す）。
const Map<String, String> _fixedTexts = {
  'INVALID_PATH': 'その名前・場所は使えない',
  'OUTSIDE_VAULT': '保管庫の外を指している（リンクの先など）',
  'RESERVED_PATH': '.vault は管理用の名前なので使えない',
  'INVALID_ROOT': 'その場所は保管庫にできない（ドライブ文字から始まる絶対パスで、保管庫にできる場所を選ぶ）',
  'NOT_A_VAULT': '保管庫でない（はじめてなら「作って開く」を選ぶ）',
  'LOCKED': 'ほかのアプリが書き込み中。少し待ってから、もう一度',
  'INVALID_ARGUMENT': 'アプリの内部の誤り（引数）',
  'IO_ERROR': '読み書きできなかった',
  'INTERNAL': '内部のエラー。保管庫を閉じて、開き直す',
  'NOT_FOUND': '見つからない',
  'NOT_A_DIRECTORY': 'フォルダでない',
  'NOT_A_FILE': 'ファイルでない',
  'NOT_UTF8': '文字のファイルでない（UTF-8 でない）ので、表示できない',
  'TOO_LARGE': '大きすぎる',
  'ALREADY_EXISTS': '同じ名前のものが、すでにある',
  'CONFLICT': 'ほかで変わったか、戻す先にすでに何かある',
  'DISK_FULL': 'ディスクがいっぱい',
  'BUSY': 'ほかのアプリが使っている。閉じてから、もう一度',
  'READ_ONLY': '読み取り専用で開いている',
  'UNKNOWN_TOOL': 'アプリの内部の誤り（ツール）',
  'NOT_IMPLEMENTED': 'まだ作っていない',
  'ACCESS_DENIED': '権限がない（読み取り専用の印が付いているなど）',
  'DIR_NOT_EMPTY': 'フォルダが空でない',
  'INVALID_TRASH_ENTRY': 'ゴミ箱の記録が壊れている',
};

/// 画面に出す文。コードごとの決まった文に、vault-core の説明の文（日本語で、保管庫の中の相対パスだけを含む）を添える。
///
/// ただし `INVALID_ARGUMENT`（ツールの引数の誤り。serde の文が 300 字まで入る）と `INTERNAL` は、決まった文だけにする
/// （アプリが組み立てた引数の誤りなので、利用者が直せる中身がないため）。
/// [VaultException] でない例外（DLL を読み込めないなど）は、決まった文だけにし、元の文（DLL の絶対パスなど）は出さない。
String errorText(Object error) {
  if (error is! VaultException) {
    return unexpectedErrorText;
  }
  final fixed = _fixedTexts[error.code] ?? '失敗した（${error.code}）';
  if (error.code == 'INVALID_ARGUMENT' ||
      error.code == 'INTERNAL' ||
      error.message.isEmpty) {
    return fixed;
  }
  return '$fixed：${error.message}';
}
