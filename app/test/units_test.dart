import 'dart:convert';
import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vault_app/src/errors.dart';
import 'package:vault_app/src/folder_picker.dart';
import 'package:vault_app/src/pages/browser_page.dart';
import 'package:vault_app/src/vault_api.dart';
import 'package:vault_app/src/rescan.dart';
import 'package:vault_app/src/tools.dart';

/// crates/vault-core/tool_names.json のツールの名前。
Set<String> toolNames() {
  final file = File('../crates/vault-core/tool_names.json');
  return (jsonDecode(file.readAsStringSync()) as List<Object?>)
      .cast<String>()
      .toSet();
}

/// lib/src/vault_api.dart の、VaultApi のメソッドの名前と、typedef の名前（文字として読む）。
({List<String> methods, List<String> typedefs}) vaultApiOperations() {
  final source = File(
    'lib/src/vault_api.dart',
  ).readAsStringSync().replaceAll('\r\n', '\n');
  final start = source.indexOf('abstract interface class VaultApi {');
  expect(start, isNot(-1), reason: 'VaultApi が見つからない');
  final end = source.indexOf('\n}\n', start);
  expect(end, isNot(-1), reason: 'VaultApi の終わりが見つからない');
  final body = source.substring(start, end);
  final methods = RegExp(
    r'^  Future<[^\n]*?>\s+(\w+)\s*\(',
    multiLine: true,
  ).allMatches(body).map((m) => m.group(1)!).toList();
  final typedefs = RegExp(
    r'^typedef\s+(\w+)\s*=',
    multiLine: true,
  ).allMatches(source).map((m) => m.group(1)!).toList();
  return (methods: methods, typedefs: typedefs);
}

/// 失敗した Future の例外（成功したらテストを落とす）。
Future<Object> errorOf(Future<Object?> future) async {
  try {
    final value = await future;
    fail('成功してしまった：$value');
  } on TestFailure {
    rethrow;
  } on Object catch (e) {
    return e;
  }
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  /// 網羅率：画面が使う操作（VaultApi のメソッドと、開く・作る関数の typedef）は、どれもツール（tool_names.json）か
  /// 人だけの手続き（Procedures）に当たる。手続きの名前はツールにない（作る・開く・閉じるを AI に出さない）。
  test('画面の操作は、どれもツールか人だけの手続きに当たる（手続きはツールにない）', () {
    final names = toolNames();
    final ops = vaultApiOperations();
    expect(ops.methods, hasLength(10), reason: 'メソッドを読めている');
    expect(ops.methods, containsAll(<String>['info', 'changes', 'close']));
    expect(ops.typedefs, ['VaultOpener', 'VaultCreator', 'VaultDefaultOpener']);
    final all = [...ops.methods, ...ops.typedefs];
    for (final op in all) {
      final target = apiMethodTools[op];
      expect(target, isNotNull, reason: '$op が apiMethodTools にない');
      expect(
        names.contains(target) || Procedures.all.contains(target),
        isTrue,
        reason: '$op → $target',
      );
    }
    expect(
      apiMethodTools.keys.toSet(),
      all.toSet(),
      reason: 'apiMethodTools に、もうない操作が残っていない',
    );
    for (final procedure in Procedures.all) {
      expect(names, isNot(contains(procedure)), reason: procedure);
    }
    for (final name in names) {
      expect(name, isNot(contains('init')), reason: name);
      expect(name, isNot(contains('create')), reason: name);
      expect(name, isNot(contains('default')), reason: name);
    }
    expect(apiMethodTools['VaultOpener'], Procedures.openVault);
    expect(apiMethodTools['VaultCreator'], Procedures.createVault);
    expect(apiMethodTools['VaultDefaultOpener'], Procedures.openDefaultVault);
    expect(Procedures.openDefaultVault, 'open_default_vault');
    expect(apiMethodTools['close'], Procedures.closeVault);
    expect(
      apiMethodTools.values.where(names.contains).toSet(),
      Tools.all.toSet(),
      reason: 'ツールに当たる操作は Tools.all と同じ',
    );
  });

  /// 既定の保管庫の名前は、vault-ffi の DEFAULT_VAULT_NAME（場所を決める Rust の側）と同じ（画面の文の名前がずれない）。
  test('既定の保管庫の名前（defaultVaultName）は、vault-ffi の DEFAULT_VAULT_NAME と同じ', () {
    final source = File('../crates/vault-ffi/src/lib.rs').readAsStringSync();
    final match = RegExp(
      r'pub const DEFAULT_VAULT_NAME: &str = "([^"]+)";',
    ).firstMatch(source);
    expect(match, isNotNull, reason: 'DEFAULT_VAULT_NAME が見つからない');
    expect(defaultVaultName, match!.group(1));
    expect(defaultVaultName, 'Buildoffice');
  });

  /// OpenPage の既定の値は本物でない（本物の vault-ffi の既定の保管庫を開く関数を渡すのは VaultApp だけ。画面のテストが
  /// 本物のホームに作らないため）：defaultOpener は必須の引数で既定の値がなく、openDefaultOnStart の既定は false。
  /// open_page.dart は vault-ffi を呼ぶ部品を読み込まない。
  test('OpenPage は本物の既定の保管庫を開く関数を既定にしない（必須の引数・起動で開くのは既定で false）', () {
    final source = File(
      'lib/src/pages/open_page.dart',
    ).readAsStringSync().replaceAll('\r\n', '\n');
    expect(source, contains('required this.defaultOpener,'));
    expect(source, contains('this.openDefaultOnStart = false,'));
    for (final real in [
      'openDefaultWithFfi',
      'VaultClient',
      'appDllPath',
      'vault_client.dart',
      'native.dart',
      'main.dart',
    ]) {
      expect(source, isNot(contains(real)), reason: real);
    }
    final main = File('lib/main.dart').readAsStringSync();
    expect(main, contains('this.defaultOpener = openDefaultWithFfi,'));
    expect(main, contains('this.openDefaultOnStart = true,'));
  });

  group('ChannelFolderPicker（runner のチャンネル）', () {
    const channel = MethodChannel(folderPickerChannelName);
    final messenger =
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
    tearDown(() => messenger.setMockMethodCallHandler(channel, null));

    test('選んだパスを返す。取り消しは null', () async {
      final calls = <String>[];
      final replies = <Object?>[r'D:\Vaults', null];
      messenger.setMockMethodCallHandler(channel, (call) async {
        calls.add(call.method);
        expect(call.arguments, isNull);
        return replies.removeAt(0);
      });
      const picker = ChannelFolderPicker();
      expect(await picker.pickFolder(), r'D:\Vaults');
      expect(await picker.pickFolder(), isNull);
      expect(calls, ['pickFolder', 'pickFolder']);
    });

    test('失敗（PICK_FAILED・形の違う返事）は決まった文で、元の文やパスを出さない', () async {
      final replies = <Object?>[
        PlatformException(
          code: 'PICK_FAILED',
          message: r'C:\Profiles\someone\secret-folder',
        ),
        5,
        '',
      ];
      messenger.setMockMethodCallHandler(channel, (call) async {
        final next = replies.removeAt(0);
        if (next is PlatformException) {
          throw next;
        }
        return next;
      });
      const picker = ChannelFolderPicker();
      for (var i = 0; i < 3; i++) {
        final error = await errorOf(picker.pickFolder());
        expect(error, isA<FolderPickerException>(), reason: '$i');
        final message = (error as FolderPickerException).message;
        expect(message, folderPickerFailedText);
        expect(message, isNot(contains('someone')));
        expect(error.toString(), isNot(contains('secret-folder')));
      }
    });

    test('受け口がない（runner のない環境）ときは、決まった文', () async {
      final error = await errorOf(const ChannelFolderPicker().pickFolder());
      expect(error, isA<FolderPickerException>());
      expect(
        (error as FolderPickerException).message,
        folderPickerUnavailableText,
      );
    });
  });

  /// Dart の定数 ⊆ crates/vault-core/tool_names.json。
  test('アプリが使うツールの名前は、すべて tool_names.json にある', () {
    final file = File('../crates/vault-core/tool_names.json');
    final names = (jsonDecode(file.readAsStringSync()) as List<Object?>)
        .cast<String>()
        .toSet();
    expect(names, hasLength(10));
    for (final tool in Tools.all) {
      expect(names, contains(tool), reason: tool);
    }
    expect(Tools.all.toSet(), hasLength(Tools.all.length), reason: '重なりがない');
  });

  test(
    'エラーの文：コードごとの決まった文。INVALID_ARGUMENT と INTERNAL は vault-core の文を添えない',
    () {
      expect(
        errorText(const VaultException('NOT_FOUND', 'ない：a.txt')),
        '見つからない：ない：a.txt',
      );
      expect(
        errorText(
          const VaultException(
            'INVALID_ARGUMENT',
            '引数が正しくない：unknown field `x`, expected `path`',
          ),
        ),
        'アプリの内部の誤り（引数）',
      );
      expect(
        errorText(const VaultException('INTERNAL', '内部のエラー（処理が途中で止まった）')),
        '内部のエラー。保管庫を閉じて、開き直す',
      );
      expect(
        errorText(const VaultException('SOMETHING_NEW', 'x')),
        '失敗した（SOMETHING_NEW）：x',
      );
      expect(errorText(const VaultException('READ_ONLY', '')), '読み取り専用で開いている');
      expect(errorText(StateError('閉じた')), unexpectedErrorText);
    },
  );

  test('vault-core のエラーでない例外は、決まった文だけを出し、元の文（DLL の絶対パスなど）を出さない', () {
    final error = ArgumentError(
      r"Failed to load dynamic library 'C:\Profiles\someone\app\vault_ffi.dll': error code 126",
    );
    final text = errorText(error);
    expect(text, unexpectedErrorText);
    expect(text, isNot(contains('someone')));
    expect(text, isNot(contains('vault_ffi.dll')));
  });

  test('走査の間隔：5 秒を基本に、前の走査の 10 倍まで延ばす（10 分まで）', () {
    expect(nextRescanDelay(Duration.zero), const Duration(seconds: 5));
    expect(
      nextRescanDelay(const Duration(milliseconds: 30)),
      const Duration(seconds: 5),
    );
    expect(
      nextRescanDelay(const Duration(seconds: 11)),
      const Duration(seconds: 110),
    );
    expect(
      nextRescanDelay(const Duration(minutes: 5)),
      const Duration(minutes: 10),
    );
  });

  test('一覧が一部だけのときの文：理由ごとに、件数を添える（名前は入れない）', () {
    const none = EntryList(entries: []);
    expect(none.partial, isFalse);

    const truncated = EntryList(entries: [], truncated: true);
    expect(truncated.partial, isTrue);
    expect(partialListText(truncated), '一部だけ表示しています（多すぎるので、はじめの部分だけ）');

    const overLimit = EntryList(entries: [], skippedOverLimit: 3);
    expect(overLimit.partial, isTrue);
    expect(partialListText(overLimit), '一部だけ表示しています（深すぎる・長すぎる場所のもの（3 件）を除いて）');

    const invalid = EntryList(entries: [], skippedInvalidNames: 2);
    expect(invalid.partial, isTrue);
    expect(partialListText(invalid), '一部だけ表示しています（保管庫で扱えない名前のもの（2 件）を除いて）');

    const all = EntryList(
      entries: [],
      truncated: true,
      skippedOverLimit: 1,
      skippedInvalidNames: 4,
    );
    expect(all.partial, isTrue);
    expect(
      partialListText(all),
      '一部だけ表示しています（多すぎるので、はじめの部分だけ、'
      '深すぎる・長すぎる場所のもの（1 件）を除いて、保管庫で扱えない名前のもの（4 件）を除いて）',
    );
  });

  test('続けて失敗したときの間隔：失敗のたびに倍（10 分まで）', () {
    const five = Duration(seconds: 5);
    expect(backoffRescanDelay(five, 0), five);
    expect(backoffRescanDelay(five, 1), const Duration(seconds: 10));
    expect(backoffRescanDelay(five, 3), const Duration(seconds: 40));
    expect(backoffRescanDelay(five, 20), const Duration(minutes: 10));
    expect(
      backoffRescanDelay(const Duration(minutes: 8), 1),
      const Duration(minutes: 10),
    );
  });
}
