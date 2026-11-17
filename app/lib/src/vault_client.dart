/// vault-ffi で保管庫を操作する（[VaultApi] の本物）。
///
/// - どの呼び出しも [Isolate.run] で UI のスレッドの外に出す。DLL とバッファは、その isolate の中だけで扱い、
///   isolate のあいだでは文字（JSON）と整数だけを受け渡す（ポインタを渡さない）。
/// - actor は vault-ffi が `{user, app}` にする。
library;

import 'dart:convert';
import 'dart:io';
import 'dart:isolate';

import 'errors.dart';
import 'native.dart';
import 'tools.dart';
import 'vault_api.dart';

/// アプリの exe と同じフォルダの vault_ffi.dll の絶対パス（exe のフォルダからの絶対パスで読み込む）。
String appDllPath() {
  final exeDir = File(Platform.resolvedExecutable).parent.absolute.path;
  return '$exeDir${Platform.pathSeparator}vault_ffi.dll';
}

/// vault-ffi で開いた保管庫。
final class VaultClient implements VaultApi {
  VaultClient._(this._dllPath, this._handle);

  final String _dllPath;
  int? _handle;

  /// 保管庫を開く（`init` なら作ってから開く）。`root` は、ドライブ文字から始まる絶対パス（例：`C:\Vaults\MyVault`）。
  /// 確かめるのは vault-core（Dart では確かめない）。
  static Future<VaultClient> open({
    required String dllPath,
    required String root,
    required bool init,
    bool readOnly = false,
  }) async {
    final request = jsonEncode({
      'root': root,
      'init': init,
      'read_only': readOnly,
    });
    final reply = await Isolate.run(
      () => VaultLibrary.open(dllPath).open(request),
    );
    final ok = _unwrap(reply);
    final handle = (ok['handle']! as num).toInt();
    return VaultClient._(dllPath, handle);
  }

  /// ツールを呼ぶ。返り値はツールの返り値（JSON のオブジェクト）。失敗は [VaultException]。
  Future<Map<String, Object?>> call(
    String tool,
    Map<String, Object?> args,
  ) async {
    final handle = _handle;
    if (handle == null) {
      throw StateError('閉じた保管庫は使えない');
    }
    final dllPath = _dllPath;
    final request = jsonEncode({'tool': tool, 'args': args});
    final reply = await Isolate.run(
      () => VaultLibrary.open(dllPath).call(handle, request),
    );
    return _unwrap(reply);
  }

  @override
  Future<VaultInfo> info() async {
    final ok = await call(Tools.info, {});
    return VaultInfo(
      name: ok['name']! as String,
      readOnly: ok['read_only']! as bool,
    );
  }

  @override
  Future<EntryList> list(String path) async {
    final ok = await call(Tools.list, {'path': path, 'limit': 10000});
    return EntryList(
      entries: (ok['entries']! as List<Object?>)
          .map((entry) => VaultEntry.fromJson(entry! as Map<String, Object?>))
          .toList(),
      truncated: ok['truncated']! as bool,
      skippedOverLimit: (ok['skipped_over_limit']! as num).toInt(),
      skippedInvalidNames: (ok['skipped_invalid_names']! as num).toInt(),
    );
  }

  @override
  Future<FileText> readText(String path) async {
    final ok = await call(Tools.read, {'path': path, 'encoding': 'utf8'});
    return FileText(
      content: ok['content']! as String,
      truncated: ok['truncated']! as bool,
    );
  }

  @override
  Future<void> createFile(String path, String content) async {
    await call(Tools.write, {
      'path': path,
      'content': content,
      'overwrite': false,
      'create_dirs': false,
    });
  }

  @override
  Future<void> createFolder(String path) async {
    await call(Tools.mkdir, {'path': path});
  }

  @override
  Future<String> delete(String path, {required bool recursive}) async {
    final ok = await call(Tools.delete, {'path': path, 'recursive': recursive});
    return ok['trash_id']! as String;
  }

  @override
  Future<TrashList> trashList() async {
    final ok = await call(Tools.trashList, {'limit': 10000});
    return TrashList(
      items: (ok['items']! as List<Object?>)
          .map((item) => TrashItem.fromJson(item! as Map<String, Object?>))
          .toList(),
      truncated: ok['truncated']! as bool,
    );
  }

  @override
  Future<String> restore(String trashId) async {
    final ok = await call(Tools.restore, {'trash_id': trashId});
    return ok['path']! as String;
  }

  @override
  Future<ChangesPage> changes({
    required int sinceSeq,
    required bool rescan,
    int limit = 200,
  }) async {
    final ok = await call(Tools.changes, {
      'since_seq': sinceSeq,
      'rescan': rescan,
      'limit': limit,
    });
    return ChangesPage(
      count: (ok['changes']! as List<Object?>).length,
      nextSeq: (ok['next_seq']! as num).toInt(),
      truncated: ok['truncated']! as bool,
      scanned: ok['scanned']! as bool,
    );
  }

  /// 閉じる。vf_close は返り値がないので、閉じたことは Dart の側で覚え、そのあとの呼び出しは
  /// vault-ffi に送らずに [StateError] にする。何度呼んでもよい。
  @override
  Future<void> close() async {
    final handle = _handle;
    if (handle == null) {
      return;
    }
    _handle = null;
    final dllPath = _dllPath;
    await Isolate.run(() => VaultLibrary.open(dllPath).close(handle));
  }
}

/// `{"ok":…}` なら中身を、`{"error":{"code","message"}}` なら [VaultException] を投げる。
Map<String, Object?> _unwrap(String reply) {
  final json = jsonDecode(reply) as Map<String, Object?>;
  final error = json['error'];
  if (error is Map<String, Object?>) {
    throw VaultException(error['code']! as String, error['message']! as String);
  }
  return json['ok']! as Map<String, Object?>;
}
