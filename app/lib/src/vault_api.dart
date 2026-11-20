/// 画面が使う保管庫の操作。中身は vault-core のツール（vault_client.dart が vault-ffi で呼ぶ）。
/// 画面のテストでは、偽のものに差し替える。パスの検証などは Dart で作り直さず、vault-core に任せる。
library;

/// 保管庫の情報（vault_info）。
class VaultInfo {
  const VaultInfo({required this.name, required this.readOnly});

  final String name;
  final bool readOnly;
}

/// 一覧の1項目（vault_list の entries）。
class VaultEntry {
  const VaultEntry({
    required this.path,
    required this.name,
    required this.kind,
    required this.size,
    required this.modified,
  });

  factory VaultEntry.fromJson(Map<String, Object?> json) => VaultEntry(
    path: json['path'] as String,
    name: json['name'] as String,
    kind: json['kind'] as String,
    size: (json['size'] as num).toInt(),
    modified: json['modified'] as String,
  );

  final String path;
  final String name;

  /// `file`・`dir`・`link`。
  final String kind;
  final int size;

  /// RFC 3339（UTC）。
  final String modified;

  bool get isDir => kind == 'dir';
}

/// ゴミ箱の1項目（vault_trash_list の items）。
class TrashItem {
  const TrashItem({
    required this.trashId,
    required this.originalPath,
    required this.kind,
    required this.size,
    required this.deletedAt,
    required this.reason,
  });

  factory TrashItem.fromJson(Map<String, Object?> json) => TrashItem(
    trashId: json['trash_id'] as String,
    originalPath: json['original_path'] as String,
    kind: json['kind'] as String,
    size: (json['size'] as num).toInt(),
    deletedAt: json['deleted_at'] as String,
    reason: json['reason'] as String,
  );

  final String trashId;
  final String originalPath;
  final String kind;
  final int size;
  final String deletedAt;
  final String reason;
}

/// フォルダの中の一覧（vault_list）。一部だけのとき（[partial]）は、画面に決まった文を出す。
class EntryList {
  const EntryList({
    required this.entries,
    this.truncated = false,
    this.skippedOverLimit = 0,
    this.skippedInvalidNames = 0,
  });

  final List<VaultEntry> entries;

  /// 多すぎるので、途中で切ったか。
  final bool truncated;

  /// 深すぎる・長すぎる場所なので、出さなかった数。
  final int skippedOverLimit;

  /// 保管庫で扱えない名前なので、出さなかった数。
  final int skippedInvalidNames;

  /// 一部だけを表示しているか。
  bool get partial =>
      truncated || skippedOverLimit > 0 || skippedInvalidNames > 0;
}

/// ゴミ箱の一覧（vault_trash_list）。
class TrashList {
  const TrashList({required this.items, this.truncated = false});

  final List<TrashItem> items;

  /// 多すぎるので、新しいものから途中までで切ったか。
  final bool truncated;
}

/// ファイルの中身（vault_read）。
class FileText {
  const FileText({required this.content, required this.truncated});

  final String content;

  /// 長すぎて、途中までしか読んでいないか。
  final bool truncated;
}

/// 変更の一覧の1回ぶん（vault_changes）。
class ChangesPage {
  const ChangesPage({
    required this.count,
    required this.nextSeq,
    required this.truncated,
    required this.scanned,
  });

  /// 返ってきた変更の数。
  final int count;
  final int nextSeq;
  final bool truncated;

  /// この呼び出しで走査したか（前の走査から 2 秒以内などは、走査しない）。
  final bool scanned;
}

/// 保管庫の操作。
abstract interface class VaultApi {
  Future<VaultInfo> info();

  /// フォルダの中の一覧（フォルダを先に、名前の順）。`path` は保管庫の中の相対パス（根は ""）。
  Future<EntryList> list(String path);

  /// ファイルを文字として読む。
  Future<FileText> readText(String path);

  /// 新しいファイルを作る（同じ名前があれば ALREADY_EXISTS。上書きしない）。
  Future<void> createFile(String path, String content);

  /// 新しいフォルダを作る。
  Future<void> createFolder(String path);

  /// ゴミ箱へ移す（フォルダは中身ごと）。返り値は trash_id。
  Future<String> delete(String path, {required bool recursive});

  Future<TrashList> trashList();

  /// ゴミ箱から、もとの場所へ戻す。返り値は戻した場所。
  Future<String> restore(String trashId);

  /// 変更の一覧（`rescan` なら、先に保管庫を調べる）。
  Future<ChangesPage> changes({
    required int sinceSeq,
    required bool rescan,
    int limit = 200,
  });

  /// 閉じる（そのあとは使えない）。
  Future<void> close();
}

/// 保管庫を開く関数（`init` なら作ってから開く）。人だけの手続き（ツールにしない）。
typedef VaultOpener =
    Future<VaultApi> Function({
      required String root,
      required bool init,
      required bool readOnly,
    });

/// 置き場所（`parent`）の中に、名前（`name`）を付けて保管庫を作り、書ける状態で開く関数。人だけの手続き（ツールにしない）。
typedef VaultCreator =
    Future<VaultApi> Function({required String parent, required String name});

/// 既定の保管庫の名前（ホームのフォルダの直下のフォルダ）。画面の文にだけ使い、パスは組み立てない
/// （場所を決めて作る・開くのは vault-core）。vault-ffi の `DEFAULT_VAULT_NAME` と同じことを units_test が確かめる。
const String defaultVaultName = 'Buildoffice';

/// 既定の保管庫を開いた結果。
class OpenedDefault {
  const OpenedDefault({required this.api, required this.created});

  /// 開いた保管庫（書ける状態）。
  final VaultApi api;

  /// この呼び出しで作ったか（なかったので作った）。
  final bool created;
}

/// 既定の保管庫（ホームのフォルダの直下の [defaultVaultName]）を、書ける状態で開く関数（なければ作る）。
/// 人だけの手続き（ツールにしない）。
typedef VaultDefaultOpener = Future<OpenedDefault> Function();
