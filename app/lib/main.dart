/// 保管庫のファイルブラウザ（Phase 0。Windows）。表示と入力だけを受け持ち、ロジックは Rust のコア（vault-ffi）にある。
library;

import 'package:flutter/material.dart';

import 'src/pages/open_page.dart';
import 'src/vault_api.dart';
import 'src/vault_client.dart';

void main() {
  runApp(const VaultApp());
}

/// アプリ。
class VaultApp extends StatelessWidget {
  const VaultApp({super.key, this.opener = openWithFfi});

  /// 保管庫を開く関数（本物は vault-ffi。テストでは偽のものを渡す）。
  final VaultOpener opener;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Buildoffice',
      theme: ThemeData(colorSchemeSeed: Colors.teal, useMaterial3: true),
      home: OpenPage(opener: opener),
    );
  }
}

/// exe のフォルダの vault_ffi.dll で保管庫を開く。
Future<VaultApi> openWithFfi({
  required String root,
  required bool init,
  required bool readOnly,
}) => VaultClient.open(
  dllPath: appDllPath(),
  root: root,
  init: init,
  readOnly: readOnly,
);
