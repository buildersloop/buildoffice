/// 画面のテスト（偽の保管庫で。vault-ffi は使わない）。
library;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:vault_app/src/errors.dart';
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

void main() {
  testWidgets('パスを入れて開くと、一覧が出る（パスはそのまま vault-core に渡す）', (tester) async {
    final vault = FakeVault();
    final opened = <(String, bool, bool)>[];
    await tester.pumpWidget(
      MaterialApp(
        home: OpenPage(
          autoRescan: false,
          opener: ({required root, required init, required readOnly}) async {
            opened.add((root, init, readOnly));
            return vault;
          },
        ),
      ),
    );
    await tester.enterText(
      find.byKey(const Key('root')),
      r'  C:\Vaults\MyVault ',
    );
    await tester.tap(find.text('作って開く').first);
    await tester.pump();
    await tester.tap(find.byKey(const Key('open')));
    await tester.pumpAndSettle();
    expect(opened, [(r'C:\Vaults\MyVault', true, false)]);
    expect(find.text('保管庫：MyVault'), findsOneWidget);
    expect(find.text('docs'), findsOneWidget);
    expect(find.text('a.txt'), findsOneWidget);
    // 開いたときに、履歴の最後まで読み進める（rescan:false）
    expect(vault.calls, contains('changes:0:false'));
  });

  testWidgets('開けなければ、コードごとの決まった文を出す', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: OpenPage(
          autoRescan: false,
          opener: ({required root, required init, required readOnly}) async =>
              throw const VaultException(
                'NOT_A_VAULT',
                '保管庫でない（.vault/vault.json がない）',
              ),
        ),
      ),
    );
    await tester.enterText(find.byKey(const Key('root')), r'C:\x');
    await tester.tap(find.byKey(const Key('open')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('open_error')), findsOneWidget);
    expect(find.textContaining('保管庫でない（はじめてなら'), findsOneWidget);
  });

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
