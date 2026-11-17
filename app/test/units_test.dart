import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:vault_app/src/errors.dart';
import 'package:vault_app/src/pages/browser_page.dart';
import 'package:vault_app/src/vault_api.dart';
import 'package:vault_app/src/rescan.dart';
import 'package:vault_app/src/tools.dart';

void main() {
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
