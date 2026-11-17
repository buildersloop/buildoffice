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
