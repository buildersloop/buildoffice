/// 保管庫のファイルブラウザ（Phase 0。Windows）。表示と入力だけを受け持ち、ロジックは Rust のコア（vault-ffi）にある。
library;

import 'package:flutter/material.dart';

import 'src/folder_picker.dart';
import 'src/pages/open_page.dart';
import 'src/vault_api.dart';
import 'src/vault_client.dart';

void main() {
  runApp(const VaultApp());
}

/// アプリ。起動のたびに、既定の保管庫（ホームのフォルダの直下の Buildoffice）を開く（なければ作る）。
class VaultApp extends StatelessWidget {
  const VaultApp({
    super.key,
    this.opener = openWithFfi,
    this.creator = createWithFfi,
    this.defaultOpener = openDefaultWithFfi,
    this.openDefaultOnStart = true,
    this.picker = const ChannelFolderPicker(),
  });

  /// 保管庫を開く関数（本物は vault-ffi。テストでは偽のものを渡す）。
  final VaultOpener opener;

  /// 保管庫を作って開く関数（本物は vault-ffi。テストでは偽のものを渡す）。
  final VaultCreator creator;

  /// 既定の保管庫を開く関数（なければ作る。本物は vault-ffi）。本物を OpenPage に渡すのは、ここだけ。
  final VaultDefaultOpener defaultOpener;

  /// 起動で（開く画面が最初にできたときに1回だけ）、既定の保管庫を開くか。
  final bool openDefaultOnStart;

  /// フォルダを選ぶ窓（本物は runner のチャンネル。テストでは偽のものを渡す）。
  final FolderPicker picker;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Buildoffice',
      theme: ThemeData(colorSchemeSeed: Colors.teal, useMaterial3: true),
      home: OpenPage(
        opener: opener,
        creator: creator,
        defaultOpener: defaultOpener,
        openDefaultOnStart: openDefaultOnStart,
        picker: picker,
      ),
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

/// exe のフォルダの vault_ffi.dll で、置き場所の中に保管庫を作って開く。
Future<VaultApi> createWithFfi({
  required String parent,
  required String name,
}) => VaultClient.create(dllPath: appDllPath(), parent: parent, name: name);

/// exe のフォルダの vault_ffi.dll で、既定の保管庫（ホームのフォルダの直下の Buildoffice）を開く（なければ作る）。
/// 場所を決めるのは vault-core（Dart はパスを渡さない）。
Future<OpenedDefault> openDefaultWithFfi() =>
    VaultClient.openDefault(dllPath: appDllPath());
