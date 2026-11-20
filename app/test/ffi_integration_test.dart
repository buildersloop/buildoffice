/// Dart から release の vault_ffi.dll を呼ぶ結合テスト（`cargo build --release -p vault-ffi` が先に要る）。
/// 保管庫は、リポジトリの target/flutter-tests の下に作る（%TEMP% は保管庫にできない場所）。
library;

import 'dart:convert';
import 'dart:ffi';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:vault_app/src/errors.dart';
import 'package:vault_app/src/native.dart';
import 'package:vault_app/src/pages/browser_page.dart';
import 'package:vault_app/src/vault_api.dart';
import 'package:vault_app/src/vault_client.dart';

final Directory repo = Directory.current.absolute.parent;
final String dllPath = [
  repo.path,
  'target',
  'release',
  'vault_ffi.dll',
].join(Platform.pathSeparator);
final bool dllExists = File(dllPath).existsSync();

/// VAULT_FFI_REQUIRED=1 のときは、DLL がなくても飛ばさない（下の setUpAll で失敗にする）。
final bool dllRequired = Platform.environment['VAULT_FFI_REQUIRED'] == '1';
final Object skipReason = dllExists || dllRequired
    ? false
    : 'release の vault_ffi.dll がない（cargo build --release -p vault-ffi を先に）';

int _seq = 0;

/// テストごとの場所（ドライブ文字から始まる絶対パス）。
Directory testDir(String name) {
  final dir = Directory(
    [
      repo.path,
      'target',
      'flutter-tests',
      '$name-$pid-${DateTime.now().microsecondsSinceEpoch}-${_seq++}',
    ].join(Platform.pathSeparator),
  )..createSync(recursive: true);
  addTearDown(() {
    if (dir.existsSync()) {
      dir.deleteSync(recursive: true);
    }
  });
  return dir;
}

/// 本物のホームの既定の保管庫の場所（%USERPROFILE% の下の Buildoffice）の有無と種類（読むだけ。リンクをたどらない）。
FileSystemEntityType realDefaultPlace() {
  final home = Platform.environment['USERPROFILE'];
  if (home == null || home.isEmpty) {
    fail('USERPROFILE がない');
  }
  return FileSystemEntity.typeSync(
    '$home${Platform.pathSeparator}$defaultVaultName',
    followLinks: false,
  );
}

/// 本物のホームの既定の保管庫の場所がすでにあるとき（種類は問わない。リンクも、調べられないとき＝typeSync が
/// 例外を投げたときも「ある」とみなす。Rust のテストと同じ、安全な側）、「SKIP：…」を出してこのテストを飛ばした印を
/// 付け、true を返す。使っている人の本物の保管庫を、このテストが開かないため。
bool skipWhenRealDefaultExists() {
  try {
    if (realDefaultPlace() == FileSystemEntityType.notFound) {
      return false;
    }
  } on FileSystemException {
    // 調べられない：「ある」とみなして飛ばす
  }
  const message = 'SKIP：既定の保管庫がすでにあるので、本物のホームを使うテストを飛ばした';
  stderr.writeln(message);
  markTestSkipped(message);
  return true;
}

typedef _AllocNative = Pointer<Uint8> Function(Size len);
typedef _AllocDart = Pointer<Uint8> Function(int len);
typedef _FreeNative = Void Function(Pointer<Uint8> ptr, Size len);
typedef _FreeDart = void Function(Pointer<Uint8> ptr, int len);
typedef _SendNative =
    Pointer<Uint8> Function(Pointer<Uint8> req, Size len, Pointer<Size> outLen);
typedef _SendDart =
    Pointer<Uint8> Function(Pointer<Uint8> req, int len, Pointer<Size> outLen);

/// 形の誤りの要求を vf_open_default に送る（テストの中で dart:ffi を直接引く。アプリの VaultLibrary は `{}` しか
/// 送らないので、テストだけの入口は作らない）。返り値の JSON を返す。バッファはどれも1回だけ vf_free する。
Map<String, Object?> sendDefaultRequest(List<int> bytes) {
  final lib = DynamicLibrary.open(dllPath);
  final alloc = lib.lookupFunction<_AllocNative, _AllocDart>('vf_alloc');
  final free = lib.lookupFunction<_FreeNative, _FreeDart>('vf_free');
  final send = lib.lookupFunction<_SendNative, _SendDart>('vf_open_default');
  final request = bytes.isEmpty ? nullptr : alloc(bytes.length);
  final outLen = alloc(sizeOf<Size>());
  try {
    expect(outLen, isNot(nullptr));
    if (bytes.isNotEmpty) {
      expect(request, isNot(nullptr));
      request.asTypedList(bytes.length).setAll(0, bytes);
    }
    final outLenPointer = outLen.cast<Size>();
    expect(outLenPointer.address % sizeOf<Size>(), 0);
    outLenPointer.value = 0;
    final reply = send(request, bytes.length, outLenPointer);
    final length = outLenPointer.value;
    expect(reply, isNot(nullptr));
    try {
      return jsonDecode(utf8.decode(reply.asTypedList(length)))
          as Map<String, Object?>;
    } finally {
      free(reply, length);
    }
  } finally {
    free(outLen, sizeOf<Size>());
    if (bytes.isNotEmpty) {
      free(request, bytes.length);
    }
  }
}

Future<String> errorCodeOf(Future<Object?> future) async {
  try {
    await future;
  } on VaultException catch (e) {
    return e.code;
  }
  fail('成功してしまった');
}

void main() {
  setUpAll(() {
    if (dllRequired && !dllExists) {
      fail(
        'VAULT_FFI_REQUIRED=1 なのに、release の vault_ffi.dll がない（$dllPath）。'
        'cargo build --release -p vault-ffi を先に',
      );
    }
  });

  test('open(init) → 9つのツール → close', () async {
    final root = '${testDir('ffi-flow').path}${Platform.pathSeparator}v';
    final client = await VaultClient.open(
      dllPath: dllPath,
      root: root,
      init: true,
    );
    addTearDown(client.close);
    final info = await client.info();
    expect(info.name, 'v');
    expect(info.readOnly, isFalse);
    await client.createFolder('notes');
    await client.createFile('notes/a.txt', 'こんにちは');
    final listed = await client.list('notes');
    expect(listed.entries.map((e) => e.name), ['a.txt']);
    expect(listed.partial, isFalse);
    final text = await client.readText('notes/a.txt');
    expect(text.content, 'こんにちは');
    final trashId = await client.delete('notes', recursive: true);
    final trash = await client.trashList();
    expect(trash.items.single.trashId, trashId);
    expect(trash.items.single.originalPath, 'notes');
    expect(trash.truncated, isFalse);
    expect(await client.restore(trashId), 'notes');
    expect((await client.readText('notes/a.txt')).content, 'こんにちは');
    final changes = await client.changes(sinceSeq: 0, rescan: true);
    expect(changes.count, greaterThanOrEqualTo(5));
    await client.close();
    await client.close(); // 何度でもよい
    expect(
      () => client.info(),
      throwsStateError,
      reason: '閉じたあとは vault-ffi に送らない',
    );

    // 履歴の actor は {user, app}
    final journal =
        Directory([root, '.vault', 'journal'].join(Platform.pathSeparator))
            .listSync()
            .whereType<File>()
            .expand((f) => f.readAsLinesSync())
            .map((line) => jsonDecode(line) as Map<String, Object?>);
    for (final entry in journal) {
      final actor = entry['actor']! as Map<String, Object?>;
      expect(actor['kind'], 'user');
      expect(actor['id'], 'app');
    }
  }, skip: skipReason);

  test('エラーの道：保管庫にできない場所・保管庫でない・ない・すでにある・閉じたハンドル', () async {
    final dir = testDir('ffi-errors');
    final sep = Platform.pathSeparator;
    // 相対・UNC・\\?\ の形は INVALID_ROOT（Dart では確かめず、vault-core が断る）
    for (final root in [
      'relative${sep}v',
      r'\\vault-test-no-such-server\share\v',
      '\\\\?\\${dir.path}${sep}x',
    ]) {
      expect(
        await errorCodeOf(
          VaultClient.open(dllPath: dllPath, root: root, init: true),
        ),
        'INVALID_ROOT',
        reason: root,
      );
    }
    // 保管庫でないフォルダを開く
    expect(
      await errorCodeOf(
        VaultClient.open(dllPath: dllPath, root: dir.path, init: false),
      ),
      'NOT_A_VAULT',
    );
    final client = await VaultClient.open(
      dllPath: dllPath,
      root: '${dir.path}${sep}v',
      init: true,
    );
    addTearDown(client.close);
    expect(await errorCodeOf(client.readText('missing.txt')), 'NOT_FOUND');
    await client.createFile('a.txt', 'a');
    expect(
      await errorCodeOf(client.createFile('a.txt', 'b')),
      'ALREADY_EXISTS',
    );
    expect(
      await errorCodeOf(client.createFile('../x.txt', 'x')),
      'INVALID_PATH',
    );
    expect(await errorCodeOf(client.call('vault_nothing', {})), 'UNKNOWN_TOOL');
    // 読み取り専用で開く
    final readOnly = await VaultClient.open(
      dllPath: dllPath,
      root: '${dir.path}${sep}v',
      init: false,
      readOnly: true,
    );
    addTearDown(readOnly.close);
    expect(await errorCodeOf(readOnly.createFile('b.txt', 'b')), 'READ_ONLY');
    await readOnly.close();
    await client.close();

    // 閉じたハンドルを直に送ると、vault-ffi は INVALID_ARGUMENT
    final library = VaultLibrary.open(dllPath);
    final reply =
        jsonDecode(library.call(987654321, jsonEncode({'tool': 'vault_info'})))
            as Map<String, Object?>;
    expect(
      (reply['error']! as Map<String, Object?>)['code'],
      'INVALID_ARGUMENT',
    );
  }, skip: skipReason);

  test('ツールで扱えない名前のファイルは、一覧に出ず、数だけが出る（名前は画面の文に出ない）', () async {
    final sep = Platform.pathSeparator;
    final root = '${testDir('ffi-invalid-name').path}${sep}v';
    final client = await VaultClient.open(
      dllPath: dllPath,
      root: root,
      init: true,
    );
    addTearDown(client.close);
    await client.createFile('ok.txt', 'ok');
    // 外から（dart:io で）、~ と数字の名前のファイルを作る（vault-core の文字列の規則では使えない名前）
    File('$root${sep}secret~1.txt').writeAsStringSync('x');
    final list = await client.list('');
    expect(list.entries.map((e) => e.name), ['ok.txt']);
    expect(list.partial, isTrue);
    expect(list.skippedInvalidNames, 1);
    expect(list.skippedOverLimit, 0);
    final text = partialListText(list);
    expect(text, contains('保管庫で扱えない名前のもの（1 件）'));
    expect(text, isNot(contains('secret')));
    await client.close();
  }, skip: skipReason);

  test('create：置き場所の中に名前を付けて作り、書ける状態で開く（履歴の actor は {user, app}）', () async {
    final sep = Platform.pathSeparator;
    final dir = testDir('ffi-create');
    final client = await VaultClient.create(
      dllPath: dllPath,
      parent: dir.path,
      name: '新しい保管庫',
    );
    addTearDown(client.close);
    final info = await client.info();
    expect(info.name, '新しい保管庫');
    expect(info.readOnly, isFalse);
    await client.createFile('a.txt', 'a');
    expect((await client.readText('a.txt')).content, 'a');
    final journal =
        Directory(['${dir.path}$sep新しい保管庫', '.vault', 'journal'].join(sep))
            .listSync()
            .whereType<File>()
            .expand((f) => f.readAsLinesSync())
            .map((line) => jsonDecode(line) as Map<String, Object?>)
            .toList();
    expect(journal.first['op'], 'init');
    final actor = journal.first['actor']! as Map<String, Object?>;
    expect(actor['kind'], 'user');
    expect(actor['id'], 'app');
    await client.close();
  }, skip: skipReason);

  test('create：名前の誤り・空でないフォルダ・すでに保管庫・守る場所は INVALID_ROOT で、何も作らない', () async {
    final sep = Platform.pathSeparator;
    final dir = testDir('ffi-create-errors');
    List<String> names(Directory d) =>
        d.listSync().map((e) => e.path.split(sep).last).toList()..sort();

    // 名前の誤り（文は「保管庫の名前：」。名前は返さない）
    for (final name in ['', 'a${sep}b', 'secret-name.', 'CON', '.vault']) {
      try {
        await VaultClient.create(
          dllPath: dllPath,
          parent: dir.path,
          name: name,
        );
        fail('作れてしまった：$name');
      } on VaultException catch (e) {
        expect(e.code, 'INVALID_ROOT', reason: name);
        expect(e.message, startsWith('保管庫の名前：'), reason: name);
        expect(e.message, isNot(contains('secret-name')));
      }
      expect(names(dir), isEmpty, reason: name);
    }

    // 空でないフォルダ（中身は変わらない）
    final full = Directory('${dir.path}${sep}full')..createSync();
    File('${full.path}$sep.hidden').writeAsStringSync('keep');
    expect(
      await errorCodeOf(
        VaultClient.create(dllPath: dllPath, parent: dir.path, name: 'full'),
      ),
      'INVALID_ROOT',
    );
    expect(names(full), ['.hidden']);
    expect(File('${full.path}$sep.hidden').readAsStringSync(), 'keep');

    // すでに保管庫・保管庫の中
    final made = await VaultClient.create(
      dllPath: dllPath,
      parent: dir.path,
      name: 'v',
    );
    await made.close();
    expect(
      await errorCodeOf(
        VaultClient.create(dllPath: dllPath, parent: dir.path, name: 'v'),
      ),
      'INVALID_ROOT',
    );
    expect(
      await errorCodeOf(
        VaultClient.create(
          dllPath: dllPath,
          parent: '${dir.path}${sep}v',
          name: 'inner',
        ),
      ),
      'INVALID_ROOT',
    );
    expect(Directory('${dir.path}${sep}v${sep}inner').existsSync(), isFalse);

    // LOCALAPPDATA の下（守る場所）
    final localAppData = Platform.environment['LOCALAPPDATA'];
    if (Platform.isWindows && localAppData != null) {
      final name = 'vault-app-test-should-not-exist-$pid-${_seq++}';
      expect(
        await errorCodeOf(
          VaultClient.create(
            dllPath: dllPath,
            parent: localAppData,
            name: name,
          ),
        ),
        'INVALID_ROOT',
      );
      expect(Directory('$localAppData$sep$name').existsSync(), isFalse);
    }
    expect(names(dir), ['full', 'v'], reason: '断ったときは何も作らない');
  }, skip: skipReason);

  test('create：置き場所がない・ファイルのときは INVALID_ROOT（置き場所のフォルダがない）で、何も作らない', () async {
    final sep = Platform.pathSeparator;
    final dir = testDir('ffi-create-parent');
    final file = File('${dir.path}${sep}a-file.txt')..writeAsStringSync('keep');
    List<String> names(Directory d) =>
        d.listSync().map((e) => e.path.split(sep).last).toList()..sort();

    for (final parent in [
      '${dir.path}${sep}no-such-parent',
      '${dir.path}${sep}no-such-a${sep}b',
      file.path,
      '${file.path}${sep}sub',
    ]) {
      try {
        await VaultClient.create(
          dllPath: dllPath,
          parent: parent,
          name: 'secret-vault-name',
        );
        fail('作れてしまった：$parent');
      } on VaultException catch (e) {
        expect(e.code, 'INVALID_ROOT', reason: parent);
        expect(e.message, contains('置き場所のフォルダがない'), reason: parent);
        expect(e.message, isNot(contains('secret-vault-name')));
        expect(e.message, isNot(contains(dir.path)));
        // 画面に出る文は、決まった文に vault-core の文を添えたもの（パスも名前も出ない）
        final text = errorText(e);
        expect(text, startsWith('その場所・名前では保管庫にできない'));
        expect(text, endsWith('置き場所のフォルダがない（すでにあるフォルダを選ぶ）'));
      }
      expect(names(dir), ['a-file.txt'], reason: '何も作らない：$parent');
    }
    expect(file.readAsStringSync(), 'keep');

    // 置き場所を作れば、同じ要求で作れる
    Directory('${dir.path}${sep}no-such-parent').createSync();
    final made = await VaultClient.create(
      dllPath: dllPath,
      parent: '${dir.path}${sep}no-such-parent',
      name: 'secret-vault-name',
    );
    await made.close();
  }, skip: skipReason);

  /// 既定の保管庫（vf_open_default）：形の誤りの要求は、本物の DLL でも INVALID_ARGUMENT で、渡した値を返さない。要求を
  /// 先に読むので、本物のホームの既定の場所の有無・種類は前後で同じ。成功の道（`{}`）は、本物のホームに作るので呼ばない
  /// （偽のホームでの成功の道は、vault-ffi の src のテストが見る）。
  ///
  /// DefaultRequest の決まりを変えるときはこのテストを見直す（形の誤りの要求が通るようになると本物のホームに保管庫を
  /// 作るため）。本物の既定の保管庫がすでにあるときは、「SKIP：…」を出してこのテストを飛ばす。
  test('既定の保管庫：形の誤りの要求は INVALID_ARGUMENT で、本物のホームに触れない', () {
    if (skipWhenRealDefaultExists()) {
      return;
    }
    final before = realDefaultPlace();
    final requests = <List<int>>[
      utf8.encode(jsonEncode({'x': r'C:\Profiles\someone-secret\v'})),
      utf8.encode(
        jsonEncode({'root': r'C:\Profiles\someone-secret', 'init': true}),
      ),
      utf8.encode(jsonEncode({'name': defaultVaultName})),
      utf8.encode('not json'),
      utf8.encode('[]'),
      utf8.encode('null'),
      utf8.encode('{'),
      <int>[],
      <int>[0xff, 0xfe, 0x00],
    ];
    for (final request in requests) {
      final reply = sendDefaultRequest(request);
      final error = reply['error'] as Map<String, Object?>?;
      expect(error, isNotNull, reason: '$reply');
      expect(error!['code'], 'INVALID_ARGUMENT', reason: '$reply');
      expect(jsonEncode(reply), isNot(contains('someone-secret')));
      expect(realDefaultPlace(), before, reason: '本物のホームに触れない');
    }
    // VaultLibrary は vf_open_default を見つけられる（アプリは `{}` だけを送る）
    VaultLibrary.open(dllPath);
    expect(realDefaultPlace(), before);
  }, skip: skipReason);

  test('バッファは1回だけ解放する（2回目は何もしない）', () {
    final library = VaultLibrary.open(dllPath);
    final buffer = library.allocateForTest(16);
    expect(buffer.isFreed, isFalse);
    buffer.free();
    expect(buffer.isFreed, isTrue);
    buffer.free(); // 何もしない（vf_free を2回呼ばない）
    expect(buffer.isFreed, isTrue);
    expect(() => library.allocateForTest(0), throwsA(isA<VaultException>()));
    expect(
      () => library.allocateForTest(32 * 1024 * 1024 + 1),
      throwsA(isA<VaultException>()),
    );
  }, skip: skipReason);
}
