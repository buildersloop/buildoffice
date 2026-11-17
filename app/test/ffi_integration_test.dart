/// Dart から release の vault_ffi.dll を呼ぶ結合テスト（`cargo build --release -p vault-ffi` が先に要る）。
/// 保管庫は、リポジトリの target/flutter-tests の下に作る（%TEMP% は保管庫にできない場所）。
library;

import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:vault_app/src/errors.dart';
import 'package:vault_app/src/native.dart';
import 'package:vault_app/src/pages/browser_page.dart';
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
