/// アプリが使うツールの名前（vault-core の tool_specs() の名前。この一覧 ⊆ tool_names.json を
/// テストで確かめる）。Phase 0 の画面は、移動（vault_move）を使わない。
abstract final class Tools {
  static const String info = 'vault_info';
  static const String list = 'vault_list';
  static const String read = 'vault_read';
  static const String write = 'vault_write';
  static const String mkdir = 'vault_mkdir';
  static const String delete = 'vault_delete';
  static const String trashList = 'vault_trash_list';
  static const String restore = 'vault_restore';
  static const String changes = 'vault_changes';

  /// アプリが使う名前のすべて。
  static const List<String> all = [
    info,
    list,
    read,
    write,
    mkdir,
    delete,
    trashList,
    restore,
    changes,
  ];
}

/// 人だけの手続き（ツールにしない操作）。保管庫を開く・作る・閉じる・既定の保管庫を開く（なければ作る）のは、
/// アプリの画面（と vault-mcp の起動・init）だけで行い、AI のツールには出さない。名前は tool_names.json にないことを
/// テストで確かめる。
abstract final class Procedures {
  static const String openVault = 'open_vault';
  static const String createVault = 'create_vault';
  static const String closeVault = 'close_vault';
  static const String openDefaultVault = 'open_default_vault';

  /// 手続きの名前のすべて。
  static const List<String> all = [
    openVault,
    createVault,
    closeVault,
    openDefaultVault,
  ];
}

/// 画面が使う操作（vault_api.dart の VaultApi のメソッドと、開く・作る・既定を開く関数の typedef）→ ツールか手続きの名前。
/// 網羅率のテストが、vault_api.dart の操作がどれもここにあり、名前が tool_names.json か [Procedures] にあることを
/// 確かめる（ツールのない画面だけの機能を作らないため）。
const Map<String, String> apiMethodTools = {
  'info': Tools.info,
  'list': Tools.list,
  'readText': Tools.read,
  'createFile': Tools.write,
  'createFolder': Tools.mkdir,
  'delete': Tools.delete,
  'trashList': Tools.trashList,
  'restore': Tools.restore,
  'changes': Tools.changes,
  'close': Procedures.closeVault,
  'VaultOpener': Procedures.openVault,
  'VaultCreator': Procedures.createVault,
  'VaultDefaultOpener': Procedures.openDefaultVault,
};
