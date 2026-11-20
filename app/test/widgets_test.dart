/// 画面のテスト（偽の保管庫で。vault-ffi は使わない）。
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vault_app/main.dart';
import 'package:vault_app/src/errors.dart';
import 'package:vault_app/src/folder_picker.dart';
import 'package:vault_app/src/pages/browser_page.dart';
import 'package:vault_app/src/pages/open_page.dart';
import 'package:vault_app/src/pages/trash_page.dart';
import 'package:vault_app/src/rescan.dart';
import 'package:vault_app/src/vault_api.dart';

/// 呼ばれたことを記録する、偽の保管庫。
class FakeVault implements VaultApi {
  FakeVault({this.readOnly = false});

  final bool readOnly;
  final List<String> calls = [];
  Object? failNext;

  /// 次の vault_changes で返す変更の数（自動の走査で一覧を読み直すかを試す）。
  int changesCount = 0;

  /// vault_list を一部だけにする（多すぎる・扱えない名前）。
  bool partialList = false;

  /// vault_trash_list を一部だけにする。
  bool partialTrash = false;

  /// vault_changes（rescan:true）を失敗させる。
  bool failChanges = false;

  /// vault_info を失敗させる（開いたあとの最初の読み込みの失敗）。
  bool failInfo = false;

  /// vault_changes（rescan:true）で投げる、VaultException でない例外。
  Object? changesThrows;

  final Map<String, List<VaultEntry>> folders = {
    '': [_dir('docs', 'docs'), _file('a.txt', 'a.txt')],
    'docs': [_file('docs/b.txt', 'b.txt')],
  };
  final List<TrashItem> trash = [
    const TrashItem(
      trashId: '1-0-1',
      originalPath: 'old.txt',
      kind: 'file',
      size: 3,
      deletedAt: '2026-11-16T00:00:00.000Z',
      reason: 'delete',
    ),
  ];

  static VaultEntry _dir(String path, String name) => VaultEntry(
    path: path,
    name: name,
    kind: 'dir',
    size: 0,
    modified: '2026-11-16T00:00:00.000Z',
  );

  static VaultEntry _file(String path, String name) => VaultEntry(
    path: path,
    name: name,
    kind: 'file',
    size: 5,
    modified: '2026-11-16T00:00:00.000Z',
  );

  void _maybeFail() {
    final error = failNext;
    if (error != null) {
      failNext = null;
      throw error;
    }
  }

  @override
  Future<VaultInfo> info() async {
    calls.add('info');
    if (failInfo) {
      throw const VaultException('IO_ERROR', '読めない');
    }
    return VaultInfo(name: 'MyVault', readOnly: readOnly);
  }

  @override
  Future<EntryList> list(String path) async {
    calls.add('list:$path');
    return EntryList(
      entries: folders[path] ?? const [],
      truncated: partialList,
      skippedInvalidNames: partialList ? 2 : 0,
    );
  }

  @override
  Future<FileText> readText(String path) async {
    calls.add('read:$path');
    return const FileText(content: 'hello', truncated: false);
  }

  @override
  Future<void> createFile(String path, String content) async {
    calls.add('createFile:$path:$content');
    _maybeFail();
  }

  @override
  Future<void> createFolder(String path) async {
    calls.add('createFolder:$path');
    _maybeFail();
  }

  @override
  Future<String> delete(String path, {required bool recursive}) async {
    calls.add('delete:$path:$recursive');
    return '9-0-9';
  }

  @override
  Future<TrashList> trashList() async {
    calls.add('trashList');
    return TrashList(items: trash, truncated: partialTrash);
  }

  @override
  Future<String> restore(String trashId) async {
    calls.add('restore:$trashId');
    return 'old.txt';
  }

  @override
  Future<ChangesPage> changes({
    required int sinceSeq,
    required bool rescan,
    int limit = 200,
  }) async {
    calls.add('changes:$sinceSeq:$rescan');
    if (rescan && failChanges) {
      throw const VaultException('LOCKED', 'ロックを取れない');
    }
    final thrown = changesThrows;
    if (rescan && thrown != null) {
      throw thrown;
    }
    final count = rescan ? changesCount : 0;
    changesCount = 0;
    return ChangesPage(
      count: count,
      nextSeq: 7,
      truncated: false,
      scanned: true,
    );
  }

  @override
  Future<void> close() async {
    calls.add('close');
  }
}

Future<FakeVault> pumpBrowser(
  WidgetTester tester, {
  bool readOnly = false,
}) async {
  final vault = FakeVault(readOnly: readOnly);
  await tester.pumpWidget(
    MaterialApp(home: BrowserPage(api: vault, autoRescan: false)),
  );
  await tester.pumpAndSettle();
  return vault;
}

/// 偽のフォルダの窓。`results` を順に返す（String はパス、null は取り消し、ほかは投げる）。
class FakePicker implements FolderPicker {
  FakePicker(this.results);

  final List<Object?> results;
  int calls = 0;

  @override
  Future<String?> pickFolder() async {
    calls++;
    final next = results.removeAt(0);
    if (next == null || next is String) {
      return next as String?;
    }
    throw next;
  }
}

/// 開く・作るの呼ばれ方を記録する、開く画面。
class OpenHarness {
  final FakeVault vault = FakeVault();
  final List<(String, bool, bool)> opened = [];
  final List<(String, String)> created = [];

  /// 作る関数が返すもの（null なら、すぐ [vault] を返す）。
  Future<VaultApi> Function()? createResult;

  /// 開く関数が返すもの（null なら、すぐ [vault] を返す）。
  Future<VaultApi> Function()? openResult;

  /// 既定の保管庫を開く関数が返すもの（引数は [vault]）。null のまま呼ばれたら、テストを落とす（OpenPage には既定の
  /// 値がなく、本物は VaultApp だけが渡すので、ここで本物を呼ぶことはない）。
  Future<OpenedDefault> Function(FakeVault vault)? defaultResult;

  /// 既定の保管庫を開く関数が呼ばれた回数。
  int defaultCalls = 0;

  /// 画面が最初にできたときに、既定の保管庫を開くか（OpenPage の既定と同じ false）。
  bool openDefaultOnStart = false;

  Future<void> pump(WidgetTester tester, {FolderPicker? picker}) async {
    // 3つのカードが、スクロールせずに全部見える大きさにする
    tester.view.physicalSize = const Size(1280, 2000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);
    addTearDown(() {
      if (defaultResult == null && defaultCalls > 0) {
        fail('既定の保管庫を開く関数が、呼ばれるはずのないところで呼ばれた');
      }
    });
    await tester.pumpWidget(
      MaterialApp(
        home: OpenPage(
          autoRescan: false,
          picker: picker ?? FakePicker([]),
          openDefaultOnStart: openDefaultOnStart,
          defaultOpener: () {
            defaultCalls++;
            final result = defaultResult;
            if (result == null) {
              return Future<OpenedDefault>.error(
                StateError('呼ばれるはずのない既定の保管庫を開く関数'),
              );
            }
            return result(vault);
          },
          opener: ({required root, required init, required readOnly}) async {
            opened.add((root, init, readOnly));
            return await openResult?.call() ?? vault;
          },
          creator: ({required parent, required name}) async {
            created.add((parent, name));
            return await createResult?.call() ?? vault;
          },
        ),
      ),
    );
  }
}

/// キーで探して押す。
Future<void> tapKey(WidgetTester tester, String key) =>
    tester.tap(find.byKey(Key(key)));

/// ボタンが押せるか。
bool enabled(WidgetTester tester, String key) {
  final button = tester.widget<ButtonStyleButton>(find.byKey(Key(key)));
  return button.onPressed != null;
}

/// 欄の中の文字。
String fieldText(WidgetTester tester, String key) =>
    tester.widget<TextField>(find.byKey(Key(key))).controller!.text;

void openPageTests() {
  test('アプリの既定は、本物の vault-ffi と OS のフォルダの窓', () {
    const app = VaultApp();
    expect(identical(app.opener, openWithFfi), isTrue);
    expect(identical(app.creator, createWithFfi), isTrue);
    expect(app.picker, isA<ChannelFolderPicker>());
  });

  /// 3つのカードのうち「開く」と「新しく作る」の中身を確かめる（「既定の保管庫」のカードは defaultVaultTests で確かめる）。
  testWidgets('開く画面は3つのカード（既定の保管庫・開く・新しく作る）。欄の既定の値は空', (tester) async {
    await OpenHarness().pump(tester);
    expect(find.byKey(const Key('open_card')), findsOneWidget);
    expect(find.byKey(const Key('create_card')), findsOneWidget);
    expect(find.byType(SegmentedButton<bool>), findsNothing);
    for (final key in ['root', 'parent', 'name']) {
      expect(fieldText(tester, key), isEmpty, reason: key);
    }
    expect(
      find.descendant(
        of: find.byKey(const Key('create_card')),
        matching: find.byKey(const Key('create_note')),
      ),
      findsOneWidget,
      reason: '作れない場所の短い説明',
    );
    for (final key in ['open', 'create', 'pick_root', 'pick_parent']) {
      expect(enabled(tester, key), isTrue, reason: key);
    }
  });

  testWidgets('パスを入れて開くと、一覧が出る（パスは前後の空白だけを除いて vault-core に渡す。作らない）', (
    tester,
  ) async {
    final harness = OpenHarness();
    await harness.pump(tester);
    await tester.enterText(
      find.byKey(const Key('root')),
      r'  C:\Vaults\MyVault ',
    );
    await tapKey(tester, 'read_only');
    await tester.pump();
    await tapKey(tester, 'open');
    await tester.pumpAndSettle();
    expect(harness.opened, [(r'C:\Vaults\MyVault', false, true)]);
    expect(harness.created, isEmpty);
    expect(find.text('保管庫：MyVault'), findsOneWidget);
    expect(find.text('docs'), findsOneWidget);
    expect(find.text('a.txt'), findsOneWidget);
    // 開いたときに、履歴の最後まで読み進める（rescan:false）
    expect(harness.vault.calls, contains('changes:0:false'));
  });

  testWidgets('置き場所と名前を入れて「作って開く」と、作ってそのまま一覧へ。「保管庫を作った」を知らせ、パスは出さない', (
    tester,
  ) async {
    final harness = OpenHarness();
    await harness.pump(tester);
    await tester.enterText(find.byKey(const Key('parent')), '  D:\\Vaults ');
    await tester.enterText(find.byKey(const Key('name')), ' MyVault  ');
    await tapKey(tester, 'create');
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 100));
    expect(harness.created, [(r'D:\Vaults', 'MyVault')]);
    expect(harness.opened, isEmpty, reason: '作るのは作る関数だけ');
    expect(find.text('保管庫を作った'), findsOneWidget);
    await tester.pumpAndSettle();
    expect(find.text('保管庫：MyVault'), findsOneWidget);
    expect(find.text('docs'), findsOneWidget);
    expect(
      find.textContaining(r'D:\Vaults'),
      findsNothing,
      reason: '開いたあとは名前だけ',
    );
  });

  testWidgets('名前の欄で Enter を押しても作る', (tester) async {
    final harness = OpenHarness();
    await harness.pump(tester);
    await tester.enterText(find.byKey(const Key('parent')), r'D:\Vaults');
    await tester.enterText(find.byKey(const Key('name')), 'v');
    await tester.testTextInput.receiveAction(TextInputAction.done);
    await tester.pumpAndSettle();
    expect(harness.created, [(r'D:\Vaults', 'v')]);
  });

  testWidgets('開けなければ、「開く」のカードに、コードごとの決まった文を出す', (tester) async {
    final harness = OpenHarness()
      ..openResult = () async => throw const VaultException(
        'NOT_A_VAULT',
        '保管庫でない（.vault/vault.json がない）',
      );
    await harness.pump(tester);
    await tester.enterText(find.byKey(const Key('root')), r'C:\x');
    await tapKey(tester, 'open');
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('open_error')), findsOneWidget);
    expect(find.byKey(const Key('create_error')), findsNothing);
    expect(find.textContaining('保管庫でない（はじめてなら「新しく作る」で作る）'), findsOneWidget);
  });

  testWidgets('作れなければ、「新しく作る」のカードに決まった文を出す（vault-core の文を添える。例外の文やパスは出さない）', (
    tester,
  ) async {
    final harness = OpenHarness()
      ..createResult = () async => throw const VaultException(
        'INVALID_ROOT',
        '保管庫の名前：末尾が . か空白の要素は使えない',
      );
    await harness.pump(tester);
    await tester.enterText(find.byKey(const Key('parent')), r'D:\Vaults');
    await tester.enterText(find.byKey(const Key('name')), 'x.');
    await tapKey(tester, 'create');
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('open_error')), findsNothing);
    expect(
      tester.widget<Text>(find.byKey(const Key('create_error'))).data,
      'その場所・名前では保管庫にできない（保管庫にできる場所を選び、使える名前を付ける）'
      '：保管庫の名前：末尾が . か空白の要素は使えない',
    );
    expect(find.text('保管庫を作った'), findsNothing);
    expect(
      find.byKey(const Key('create_card')),
      findsOneWidget,
      reason: '画面はそのまま',
    );

    // VaultException でない例外は、決まった文だけ（元の文の絶対パスを出さない）
    harness.createResult = () async =>
        throw StateError(r'cannot load C:\Profiles\someone\vault_ffi.dll');
    await tapKey(tester, 'create');
    await tester.pumpAndSettle();
    expect(
      tester.widget<Text>(find.byKey(const Key('create_error'))).data,
      unexpectedErrorText,
    );
    expect(find.textContaining('someone'), findsNothing);
    expect(harness.created, hasLength(2));
  });

  testWidgets('「選ぶ…」：選んだパスが欄に入る。取り消すと変わらない。選べなければ決まった文（元の文は出さない）', (
    tester,
  ) async {
    final picker = FakePicker([
      r'D:\Picked\Root',
      null,
      const FolderPickerException(folderPickerFailedText),
      r'D:\Picked\Parent',
      StateError(r'C:\Profiles\someone\secret-folder'),
      const FolderPickerException(folderPickerUnavailableText),
    ]);
    final harness = OpenHarness();
    await harness.pump(tester, picker: picker);

    await tapKey(tester, 'pick_root');
    await tester.pumpAndSettle();
    expect(fieldText(tester, 'root'), r'D:\Picked\Root');

    await tapKey(tester, 'pick_root');
    await tester.pumpAndSettle();
    expect(fieldText(tester, 'root'), r'D:\Picked\Root', reason: '取り消すと変わらない');
    expect(find.byKey(const Key('open_error')), findsNothing);

    await tapKey(tester, 'pick_root');
    await tester.pumpAndSettle();
    expect(fieldText(tester, 'root'), r'D:\Picked\Root');
    expect(
      tester.widget<Text>(find.byKey(const Key('open_error'))).data,
      folderPickerFailedText,
    );

    await tapKey(tester, 'pick_parent');
    await tester.pumpAndSettle();
    expect(fieldText(tester, 'parent'), r'D:\Picked\Parent');
    expect(fieldText(tester, 'name'), isEmpty, reason: '名前は選ばない');
    expect(
      find.byKey(const Key('open_error')),
      findsNothing,
      reason: '次の操作で消える',
    );

    await tapKey(tester, 'pick_parent');
    await tester.pumpAndSettle();
    expect(
      tester.widget<Text>(find.byKey(const Key('create_error'))).data,
      folderPickerFailedText,
    );
    expect(find.textContaining('secret-folder'), findsNothing);
    expect(fieldText(tester, 'parent'), r'D:\Picked\Parent');

    await tapKey(tester, 'pick_parent');
    await tester.pumpAndSettle();
    expect(
      tester.widget<Text>(find.byKey(const Key('create_error'))).data,
      folderPickerUnavailableText,
    );
    expect(picker.calls, 6);
    expect(harness.opened, isEmpty, reason: '選ぶだけでは開かない');
    expect(harness.created, isEmpty, reason: '選ぶだけでは作らない');
  });

  testWidgets('処理中は、どのボタンも押せない（二度押ししても作るのは1回）', (tester) async {
    final pending = Completer<VaultApi>();
    final harness = OpenHarness()..createResult = () => pending.future;
    await harness.pump(tester);
    await tester.enterText(find.byKey(const Key('parent')), r'D:\Vaults');
    await tester.enterText(find.byKey(const Key('name')), 'v');
    await tapKey(tester, 'create');
    await tester.pump();
    for (final key in [
      'open',
      'create',
      'pick_root',
      'pick_parent',
      'open_default',
    ]) {
      expect(enabled(tester, key), isFalse, reason: key);
    }
    await tapKey(tester, 'create');
    await tapKey(tester, 'open');
    await tester.pump();
    expect(harness.created, hasLength(1));
    expect(harness.opened, isEmpty);
    pending.complete(harness.vault);
    await tester.pumpAndSettle();
    expect(find.text('保管庫：MyVault'), findsOneWidget);
    expect(harness.created, hasLength(1));
  });

  testWidgets('窓を出しているあいだも、ほかのボタンは押せない', (tester) async {
    final pending = Completer<String?>();
    final picker = _PendingPicker(pending);
    final harness = OpenHarness();
    await harness.pump(tester, picker: picker);
    await tapKey(tester, 'pick_parent');
    await tester.pump();
    for (final key in ['open', 'create', 'pick_root', 'pick_parent']) {
      expect(enabled(tester, key), isFalse, reason: key);
    }
    await tapKey(tester, 'pick_parent');
    await tester.pump();
    expect(picker.calls, 1);
    pending.complete(r'D:\Vaults');
    await tester.pumpAndSettle();
    expect(fieldText(tester, 'parent'), r'D:\Vaults');
    expect(enabled(tester, 'create'), isTrue);
  });
}

/// 既定の保管庫のカード。
void defaultVaultTests() {
  test('アプリの既定は、起動で本物の vault-ffi の既定の保管庫を開く（作るだけで、画面は出さない）', () {
    const app = VaultApp();
    expect(identical(app.defaultOpener, openDefaultWithFfi), isTrue);
    expect(app.openDefaultOnStart, isTrue);
  });

  testWidgets('起動で1回呼び、作ったら一覧へ移り、「作った」を知らせる', (tester) async {
    final harness = OpenHarness()
      ..openDefaultOnStart = true
      ..defaultResult = (vault) async =>
          OpenedDefault(api: vault, created: true);
    await harness.pump(tester);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 100));
    expect(harness.defaultCalls, 1);
    expect(find.text(defaultCreatedText), findsOneWidget);
    expect(defaultCreatedText, '保管庫を作った（ホームのフォルダの中の Buildoffice）');
    await tester.pumpAndSettle();
    expect(find.text('保管庫：MyVault'), findsOneWidget);
    expect(harness.defaultCalls, 1, reason: '起動の1回だけ');
    expect(harness.opened, isEmpty);
    expect(harness.created, isEmpty);
  });

  testWidgets('作らずに開いた（created:false）ときは、知らせを出さずに一覧へ', (tester) async {
    final harness = OpenHarness()
      ..openDefaultOnStart = true
      ..defaultResult = (vault) async =>
          OpenedDefault(api: vault, created: false);
    await harness.pump(tester);
    await tester.pumpAndSettle();
    expect(harness.defaultCalls, 1);
    expect(find.text('保管庫：MyVault'), findsOneWidget);
    expect(find.text(defaultCreatedText), findsNothing);
    expect(find.byType(SnackBar), findsNothing);
  });

  testWidgets('NOT_A_VAULT・INVALID_ROOT なら開く画面に残り、既定のカードに理由が出る。ほかのカードは使える', (
    tester,
  ) async {
    final harness = OpenHarness()
      ..openDefaultOnStart = true
      ..defaultResult = (vault) async => throw const VaultException(
        'NOT_A_VAULT',
        '保管庫でない（.vault/vault.json がない）',
      );
    await harness.pump(tester);
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('default_card')), findsOneWidget);
    expect(
      tester.widget<Text>(find.byKey(const Key('default_error'))).data,
      '$defaultErrorHead$defaultNotAVaultText',
    );
    expect(find.byKey(const Key('open_error')), findsNothing);
    expect(find.byKey(const Key('create_error')), findsNothing);
    for (final key in [
      'open_default',
      'open',
      'create',
      'pick_root',
      'pick_parent',
    ]) {
      expect(enabled(tester, key), isTrue, reason: key);
    }
    // ほかのカードで開ける
    await tester.enterText(find.byKey(const Key('root')), r'D:\Vaults\Other');
    await tapKey(tester, 'open');
    await tester.pumpAndSettle();
    expect(harness.opened, [(r'D:\Vaults\Other', false, false)]);
    expect(find.text('保管庫：MyVault'), findsOneWidget);
  });

  testWidgets('INVALID_ROOT なら、決まった文に vault-core の説明の文（パスを含まない）を添える', (
    tester,
  ) async {
    final harness = OpenHarness()
      ..openDefaultOnStart = true
      ..defaultResult = (vault) async => throw const VaultException(
        'INVALID_ROOT',
        '既定の保管庫の場所がリンク（ジャンクション・シンボリックリンク）になっているので、たどらない（作らない・開かない）',
      );
    await harness.pump(tester);
    await tester.pumpAndSettle();
    expect(find.text('保管庫：MyVault'), findsNothing, reason: '開く画面に残る');
    expect(
      tester.widget<Text>(find.byKey(const Key('default_error'))).data,
      '$defaultErrorHead$defaultInvalidRootText'
      '：既定の保管庫の場所がリンク（ジャンクション・シンボリックリンク）になっているので、たどらない（作らない・開かない）',
    );
    expect(enabled(tester, 'open_default'), isTrue);
  });

  testWidgets('VaultException でない例外は、決まった文だけ（元の文の絶対パスを出さない）', (tester) async {
    final harness = OpenHarness()
      ..openDefaultOnStart = true
      ..defaultResult = (vault) async =>
          throw StateError(r'cannot load C:\Profiles\someone\vault_ffi.dll');
    await harness.pump(tester);
    await tester.pumpAndSettle();
    expect(
      tester.widget<Text>(find.byKey(const Key('default_error'))).data,
      '$defaultErrorHead$unexpectedErrorText',
    );
    expect(find.textContaining('someone'), findsNothing);
    expect(find.textContaining('vault_ffi.dll'), findsNothing);
  });

  testWidgets('開いているあいだは「既定の保管庫を開く」を押せず、二度押ししても呼ぶのは1回', (tester) async {
    final pending = Completer<OpenedDefault>();
    final harness = OpenHarness()..defaultResult = (vault) => pending.future;
    await harness.pump(tester);
    expect(enabled(tester, 'open_default'), isTrue);
    await tapKey(tester, 'open_default');
    await tester.pump();
    expect(enabled(tester, 'open_default'), isFalse);
    expect(find.byKey(const Key('default_busy')), findsOneWidget);
    await tapKey(tester, 'open_default');
    await tester.pump();
    expect(harness.defaultCalls, 1);
    pending.complete(OpenedDefault(api: harness.vault, created: false));
    await tester.pumpAndSettle();
    expect(find.text('保管庫：MyVault'), findsOneWidget);
    expect(harness.defaultCalls, 1);
  });

  testWidgets('ほかのカードの処理中は「既定の保管庫を開く」も押せない', (tester) async {
    final pending = Completer<VaultApi>();
    final harness = OpenHarness()..openResult = () => pending.future;
    await harness.pump(tester);
    await tester.enterText(find.byKey(const Key('root')), r'D:\Vaults\v');
    await tapKey(tester, 'open');
    await tester.pump();
    expect(enabled(tester, 'open_default'), isFalse);
    await tapKey(tester, 'open_default');
    await tester.pump();
    expect(harness.defaultCalls, 0);
    pending.complete(harness.vault);
    await tester.pumpAndSettle();
  });

  testWidgets('閉じて開く画面へ戻っても、自動では開き直さない。ボタンでもう一度開ける', (tester) async {
    final harness = OpenHarness()
      ..openDefaultOnStart = true
      ..defaultResult = (vault) async =>
          OpenedDefault(api: vault, created: false);
    await harness.pump(tester);
    await tester.pumpAndSettle();
    expect(harness.defaultCalls, 1);
    await tapKey(tester, 'close');
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('default_card')), findsOneWidget);
    expect(harness.vault.calls.where((c) => c == 'close'), hasLength(1));
    await tester.pump(const Duration(seconds: 5));
    expect(harness.defaultCalls, 1, reason: '戻ったときは開き直さない');
    expect(enabled(tester, 'open_default'), isTrue);
    await tapKey(tester, 'open_default');
    await tester.pumpAndSettle();
    expect(harness.defaultCalls, 2);
    expect(find.text('保管庫：MyVault'), findsOneWidget);
  });

  testWidgets('openDefaultOnStart が false（OpenPage の既定）なら呼ばない', (tester) async {
    final harness = OpenHarness();
    await harness.pump(tester);
    await tester.pumpAndSettle();
    await tester.pump(const Duration(seconds: 5));
    expect(harness.defaultCalls, 0);
    expect(find.byKey(const Key('default_card')), findsOneWidget);
    expect(find.byKey(const Key('default_busy')), findsNothing);
  });

  testWidgets('既定のカードの文に絶対パスがない。説明と、保管庫でないときの直し方の文がある', (tester) async {
    final harness = OpenHarness()
      ..defaultResult = (vault) async => throw const VaultException(
        'NOT_A_VAULT',
        '保管庫でない（.vault/vault.json がない）',
      );
    await harness.pump(tester);
    await tapKey(tester, 'open_default');
    await tester.pumpAndSettle();
    final texts = tester
        .widgetList<Text>(
          find.descendant(
            of: find.byKey(const Key('default_card')),
            matching: find.byType(Text),
          ),
        )
        .map((t) => t.data ?? '')
        .toList();
    expect(texts, contains('既定の保管庫'));
    expect(texts, contains('既定の保管庫を開く'));
    expect(texts, contains(defaultCardText));
    expect(defaultCardText, contains('起動のたびに開く'));
    expect(defaultCardText, contains('消しても次の起動で作り直す'));
    expect(defaultCardText, contains('Buildoffice'));
    final error = tester
        .widget<Text>(find.byKey(const Key('default_error')))
        .data!;
    expect(error, contains('名前を変えるか中身をほかへ移してから、もう一度押す'));
    final all = [
      ...texts,
      defaultCreatedText,
      defaultNotAVaultText,
      defaultInvalidRootText,
      defaultErrorHead,
    ];
    for (final text in all) {
      expect(text, isNot(matches(RegExp(r'[A-Za-z]:[\\/]'))), reason: text);
      expect(text, isNot(contains(r'\\')), reason: text);
      expect(text, isNot(contains('%USERPROFILE%')), reason: text);
      expect(text, isNot(contains('/Users/')), reason: text);
    }
  });

  testWidgets('起動の呼び出しが終わる前に画面が消えたら、返ってきた保管庫を閉じる', (tester) async {
    final pending = Completer<OpenedDefault>();
    final harness = OpenHarness()
      ..openDefaultOnStart = true
      ..defaultResult = (vault) => pending.future;
    await harness.pump(tester);
    await tester.pump();
    expect(harness.defaultCalls, 1);
    await tester.pumpWidget(const SizedBox());
    pending.complete(OpenedDefault(api: harness.vault, created: true));
    await tester.pumpAndSettle();
    expect(harness.vault.calls, ['close']);
  });

  testWidgets('起動の呼び出しのあいだも「開く」「新しく作る」は使える。ほかを開いたあとに返った既定の保管庫は閉じる', (
    tester,
  ) async {
    final pending = Completer<OpenedDefault>();
    final late = FakeVault();
    final harness = OpenHarness()
      ..openDefaultOnStart = true
      ..defaultResult = (vault) => pending.future;
    await harness.pump(tester);
    await tester.pump();
    expect(harness.defaultCalls, 1);
    expect(enabled(tester, 'open_default'), isFalse);
    for (final key in ['open', 'create', 'pick_root', 'pick_parent']) {
      expect(enabled(tester, key), isTrue, reason: key);
    }
    await tester.enterText(find.byKey(const Key('root')), r'D:\Vaults\Other');
    await tapKey(tester, 'open');
    await tester.pumpAndSettle();
    expect(find.text('保管庫：MyVault'), findsOneWidget);
    pending.complete(OpenedDefault(api: late, created: true));
    await tester.pumpAndSettle();
    expect(late.calls, ['close'], reason: '遅れて返った保管庫は閉じる');
    expect(find.text(defaultCreatedText), findsNothing);
    expect(
      harness.vault.calls,
      isNot(contains('close')),
      reason: '開いた保管庫はそのまま',
    );
  });

  testWidgets('既定の保管庫の一覧を出しているあいだに「開く」の保管庫が返ってきたら、一覧を重ねずに閉じる', (tester) async {
    final defaultPending = Completer<OpenedDefault>();
    final openPending = Completer<VaultApi>();
    final other = FakeVault();
    final harness = OpenHarness()
      ..openDefaultOnStart = true
      ..openResult = (() => openPending.future)
      ..defaultResult = ((vault) => defaultPending.future);
    await harness.pump(tester);
    await tester.pump();
    await tester.enterText(find.byKey(const Key('root')), r'D:\Vaults\Other');
    await tapKey(tester, 'open');
    await tester.pump();
    defaultPending.complete(OpenedDefault(api: harness.vault, created: false));
    await tester.pumpAndSettle();
    expect(find.text('保管庫：MyVault'), findsOneWidget);
    openPending.complete(other);
    await tester.pumpAndSettle();
    expect(other.calls, ['close'], reason: 'あとから返った保管庫は閉じる');
    // 一覧は1つだけ：閉じると開く画面へ戻る
    await tapKey(tester, 'close');
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('default_card')), findsOneWidget);
    expect(harness.vault.calls.where((c) => c == 'close'), hasLength(1));
  });
}

class _PendingPicker implements FolderPicker {
  _PendingPicker(this.pending);

  final Completer<String?> pending;
  int calls = 0;

  @override
  Future<String?> pickFolder() {
    calls++;
    return pending.future;
  }
}

void main() {
  openPageTests();
  defaultVaultTests();

  testWidgets('フォルダへ移動し、上へ戻る。ファイルは中身を表示する', (tester) async {
    final vault = await pumpBrowser(tester);
    await tester.tap(find.text('docs'));
    await tester.pumpAndSettle();
    expect(vault.calls, contains('list:docs'));
    expect(find.text('b.txt'), findsOneWidget);
    expect(
      tester.widget<Text>(find.byKey(const Key('current_dir'))).data,
      'docs',
    );
    await tester.tap(find.byKey(const Key('up')));
    await tester.pumpAndSettle();
    expect(find.text('a.txt'), findsOneWidget);
    await tester.tap(find.text('a.txt'));
    await tester.pumpAndSettle();
    expect(vault.calls, contains('read:a.txt'));
    expect(find.text('hello'), findsOneWidget);
  });

  testWidgets('フォルダとファイルを作る。失敗は決まった文で知らせる', (tester) async {
    final vault = await pumpBrowser(tester);
    await tester.tap(find.byKey(const Key('new_folder')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('new_folder_name')), 'photos');
    await tester.tap(find.byKey(const Key('dialog_ok')));
    await tester.pumpAndSettle();
    expect(vault.calls, contains('createFolder:photos'));
    expect(find.text('フォルダを作った'), findsOneWidget);

    vault.failNext = const VaultException('ALREADY_EXISTS', 'すでにある：memo.txt');
    await tester.tap(find.byKey(const Key('new_file')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('new_file_name')), 'memo.txt');
    await tester.enterText(find.byKey(const Key('new_file_content')), 'hi');
    await tester.tap(find.byKey(const Key('dialog_ok')));
    await tester.pumpAndSettle();
    expect(vault.calls, contains('createFile:memo.txt:hi'));
    expect(find.text('同じ名前のものが、すでにある：すでにある：memo.txt'), findsOneWidget);
  });

  testWidgets('削除は確かめてからゴミ箱へ。ゴミ箱から戻せる', (tester) async {
    final vault = await pumpBrowser(tester);
    await tester.tap(find.byKey(const Key('delete:docs')));
    await tester.pumpAndSettle();
    expect(find.textContaining('中身ごとゴミ箱へ移す'), findsOneWidget);
    await tester.tap(find.byKey(const Key('confirm_delete')));
    await tester.pumpAndSettle();
    expect(vault.calls, contains('delete:docs:true'));
    expect(find.text('ゴミ箱へ移した（ゴミ箱から戻せる）'), findsOneWidget);

    await tester.tap(find.byKey(const Key('trash')));
    await tester.pumpAndSettle();
    expect(find.text('old.txt'), findsOneWidget);
    await tester.tap(find.byKey(const Key('restore:1-0-1')));
    await tester.pumpAndSettle();
    expect(vault.calls, contains('restore:1-0-1'));
    expect(find.text('もとの場所へ戻した：old.txt'), findsOneWidget);
  });

  testWidgets('「更新」で、変更を調べる（rescan:true）', (tester) async {
    final vault = await pumpBrowser(tester);
    vault.calls.clear();
    await tester.tap(find.byKey(const Key('refresh')));
    await tester.pumpAndSettle();
    expect(vault.calls.first, 'changes:7:true');
    expect(vault.calls, contains('list:'));
  });

  testWidgets('読み取り専用なら、作る・消すのボタンを出さない', (tester) async {
    await pumpBrowser(tester, readOnly: true);
    expect(find.text('保管庫：MyVault（読み取り専用）'), findsOneWidget);
    expect(find.byKey(const Key('new_folder')), findsNothing);
    expect(find.byKey(const Key('new_file')), findsNothing);
    expect(find.byKey(const Key('delete:a.txt')), findsNothing);
  });

  testWidgets('閉じると、保管庫を閉じる', (tester) async {
    final vault = FakeVault();
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => TextButton(
            onPressed: () => Navigator.of(context).push(
              MaterialPageRoute<void>(
                builder: (_) => BrowserPage(api: vault, autoRescan: false),
              ),
            ),
            child: const Text('go'),
          ),
        ),
      ),
    );
    await tester.tap(find.text('go'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('close')));
    await tester.pumpAndSettle();
    expect(
      vault.calls.where((c) => c == 'close'),
      hasLength(1),
      reason: '1回だけ',
    );
    expect(find.text('go'), findsOneWidget);
  });

  testWidgets('自動の走査：前面のあいだだけ、間隔の関数（ふだんは nextRescanDelay）で決めた間隔で呼ぶ', (
    tester,
  ) async {
    expect(
      identical(BrowserPage(api: FakeVault()).rescanDelay, nextRescanDelay),
      isTrue,
      reason: 'ふだんは nextRescanDelay',
    );
    final vault = FakeVault();
    final lastScans = <Duration>[];
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pumpWidget(
      MaterialApp(
        home: BrowserPage(
          api: vault,
          rescanDelay: (lastScan) {
            lastScans.add(lastScan);
            return const Duration(seconds: 7);
          },
        ),
      ),
    );
    await tester.pump();
    int scans() => vault.calls.where((c) => c.endsWith(':true')).length;
    expect(scans(), 0);
    // 開いたあとの最初は、基本の 5 秒
    await tester.pump(const Duration(seconds: 4));
    expect(scans(), 0);
    await tester.pump(const Duration(seconds: 1));
    expect(scans(), 1);
    // そのあとは、間隔の関数が決めた 7 秒（関数には、前の走査の時間を渡す）
    expect(lastScans, hasLength(1));
    await tester.pump(const Duration(seconds: 6));
    expect(scans(), 1);
    await tester.pump(const Duration(seconds: 1));
    expect(scans(), 2);

    // ① 前面でなくなると、止まる
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    await tester.pump(const Duration(minutes: 5));
    expect(scans(), 2);
    // ② 前面に戻ると、すぐに走査して、また続ける
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pump(const Duration(milliseconds: 1));
    expect(scans(), 3);
    await tester.pump(const Duration(seconds: 7));
    expect(scans(), 4);

    // 画面を閉じると、タイマーも止まる
    await tester.pumpWidget(const SizedBox());
    await tester.pump(const Duration(minutes: 1));
    expect(scans(), 4);
    expect(vault.calls, contains('close'));
  });

  testWidgets('自動の走査で変更が1件以上なら、一覧を読み直す（0 件なら読み直さない）', (tester) async {
    final vault = FakeVault();
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pumpWidget(MaterialApp(home: BrowserPage(api: vault)));
    await tester.pump();
    int lists() => vault.calls.where((c) => c == 'list:').length;
    final first = lists();
    await tester.pump(const Duration(seconds: 5));
    expect(lists(), first, reason: '変更が 0 件なら読み直さない');
    vault.changesCount = 1;
    await tester.pump(const Duration(seconds: 5));
    expect(lists(), first + 1, reason: '変更があれば読み直す');
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('読み取り専用のゴミ箱では、「戻す」を表示するが押せない', (tester) async {
    await tester.pumpWidget(
      MaterialApp(home: TrashPage(api: FakeVault(), readOnly: true)),
    );
    await tester.pumpAndSettle();
    final button = tester.widget<TextButton>(
      find.byKey(const Key('restore:1-0-1')),
    );
    expect(button.onPressed, isNull);
  });

  testWidgets('一覧やゴミ箱が一部だけなら、決まった文を出す', (tester) async {
    final vault = FakeVault()..partialList = true;
    await tester.pumpWidget(
      MaterialApp(home: BrowserPage(api: vault, autoRescan: false)),
    );
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('partial_notice')), findsOneWidget);
    expect(find.textContaining('一部だけ表示しています'), findsOneWidget);
    expect(find.textContaining('（2 件）'), findsOneWidget);

    vault.partialTrash = true;
    await tester.tap(find.byKey(const Key('trash')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('trash_partial_notice')), findsOneWidget);

    // 一部でなければ出さない
    vault.partialList = false;
    await tester.pageBack();
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('partial_notice')), findsNothing);
  });

  testWidgets('自動の走査が続けて失敗したら、間隔を倍に延ばし、知らせは一度だけ。成功すれば戻る', (tester) async {
    final vault = FakeVault()..failChanges = true;
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pumpWidget(MaterialApp(home: BrowserPage(api: vault)));
    await tester.pump();
    int scans() => vault.calls.where((c) => c.endsWith(':true')).length;
    // 5 秒：1回目（失敗）→ 知らせる
    await tester.pump(const Duration(seconds: 5));
    expect(scans(), 1);
    await tester.pump(const Duration(milliseconds: 500));
    expect(find.textContaining('変更を調べられなかった'), findsOneWidget);
    // 次は 10 秒後（倍）。知らせはもう出さない
    await tester.pump(const Duration(seconds: 9));
    expect(scans(), 1);
    await tester.pump(const Duration(milliseconds: 600));
    expect(scans(), 2);
    await tester.pump(const Duration(milliseconds: 500));
    expect(find.textContaining('変更を調べられなかった'), findsNothing);
    // その次は 20 秒後
    await tester.pump(const Duration(seconds: 19));
    expect(scans(), 2);
    vault.failChanges = false;
    await tester.pump(const Duration(seconds: 1));
    expect(scans(), 3, reason: '20 秒後に成功');
    // 成功したので、5 秒に戻る
    await tester.pump(const Duration(seconds: 5));
    expect(scans(), 4);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('開いたあとの最初の読み込みが失敗したら、走査の予定を入れない（前面に戻っても）', (tester) async {
    final vault = FakeVault()..failInfo = true;
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pumpWidget(MaterialApp(home: BrowserPage(api: vault)));
    await tester.pump();
    expect(find.byKey(const Key('browser_error')), findsOneWidget);
    await tester.pump(const Duration(minutes: 5));
    expect(vault.calls.where((c) => c.endsWith(':true')), isEmpty);
    // 背面 → 前面（Windows では、窓を切り替えるだけで起きる）でも、予定を入れない
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    await tester.pump(const Duration(seconds: 1));
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pump(const Duration(minutes: 5));
    expect(vault.calls.where((c) => c.endsWith(':true')), isEmpty);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('「更新」のボタンの失敗は、自動の走査の失敗に数えない（間隔を倍にしない）', (tester) async {
    final vault = FakeVault();
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pumpWidget(MaterialApp(home: BrowserPage(api: vault)));
    await tester.pump();
    int scans() => vault.calls.where((c) => c.endsWith(':true')).length;
    vault.failChanges = true;
    await tester.tap(find.byKey(const Key('refresh')));
    await tester.pump(const Duration(milliseconds: 10));
    expect(scans(), 1);
    expect(find.textContaining('ロックを取れない'), findsOneWidget, reason: 'その場で知らせる');
    vault.failChanges = false;
    // 失敗に数えていないので、次の自動の走査は 5 秒後（倍の 10 秒ではない）
    await tester.pump(const Duration(seconds: 5));
    expect(scans(), 2);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('自動の走査で VaultException でない例外が出ても、元の文（絶対パスなど）を知らせに出さない', (
    tester,
  ) async {
    final vault = FakeVault()
      ..changesThrows = StateError(
        r'cannot open C:\Profiles\someone\Vaults\secret-vault\.vault\lock',
      );
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pumpWidget(MaterialApp(home: BrowserPage(api: vault)));
    await tester.pump();
    await tester.pump(const Duration(seconds: 5));
    await tester.pump(const Duration(milliseconds: 500));
    expect(find.textContaining('変更を調べられなかった'), findsOneWidget);
    expect(find.textContaining(unexpectedErrorText), findsOneWidget);
    expect(find.textContaining('someone'), findsNothing);
    expect(find.textContaining('secret-vault'), findsNothing);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('自動の走査で2回失敗したあと、「更新」が成功したら、失敗の回数は0に戻り、次の自動の走査は5秒後', (
    tester,
  ) async {
    final vault = FakeVault()..failChanges = true;
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pumpWidget(MaterialApp(home: BrowserPage(api: vault)));
    await tester.pump();
    int scans() => vault.calls.where((c) => c.endsWith(':true')).length;
    // 5 秒後に1回目の失敗、その 10 秒後に2回目の失敗（次の予定は 20 秒後）
    await tester.pump(const Duration(seconds: 5));
    expect(scans(), 1);
    await tester.pump(const Duration(seconds: 10));
    expect(scans(), 2);
    // 成功する状態にして「更新」を押す
    vault.failChanges = false;
    await tester.tap(find.byKey(const Key('refresh')));
    await tester.pump(const Duration(milliseconds: 10));
    expect(scans(), 3);
    // 失敗の回数が 0 に戻ったので、次の自動の走査は 5 秒後（倍にした 20 秒後ではない）
    await tester.pump(const Duration(seconds: 4));
    expect(scans(), 3);
    await tester.pump(const Duration(seconds: 1));
    expect(scans(), 4);
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('失敗が続いているあいだに前面に戻ると、すぐ1回調べ、知らせは2回目を出さない', (tester) async {
    final vault = FakeVault()..failChanges = true;
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pumpWidget(MaterialApp(home: BrowserPage(api: vault)));
    await tester.pump();
    int scans() => vault.calls.where((c) => c.endsWith(':true')).length;
    const notice = '変更を調べられなかった';
    // 5 秒後に1回目の失敗 → 知らせる（次の予定は 10 秒後）
    await tester.pump(const Duration(seconds: 5));
    expect(scans(), 1);
    await tester.pump(const Duration(milliseconds: 500));
    expect(find.textContaining(notice), findsOneWidget);
    // 知らせが消えるまで待つ（次の予定より前）
    await tester.pump(const Duration(seconds: 5));
    await tester.pump(const Duration(seconds: 1));
    expect(find.textContaining(notice), findsNothing);
    expect(scans(), 1);
    // 背面 → 前面：失敗が続いていても、すぐ1回調べる
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
    await tester.pump(const Duration(milliseconds: 500));
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pump(const Duration(milliseconds: 1));
    expect(scans(), 2, reason: '前面に戻ったら、すぐ調べる');
    // また失敗したが、知らせは2回目を出さない
    await tester.pump(const Duration(milliseconds: 500));
    expect(find.textContaining(notice), findsNothing);
    await tester.pumpWidget(const SizedBox());
  });
}
