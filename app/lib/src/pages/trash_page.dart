import 'package:flutter/material.dart';

import '../errors.dart';
import '../vault_api.dart';

/// ゴミ箱の一覧と復元（vault_trash_list・vault_restore）。新しい順。
/// 読み取り専用のときは、「戻す」を表示するが押せない。
class TrashPage extends StatefulWidget {
  const TrashPage({super.key, required this.api, required this.readOnly});

  final VaultApi api;
  final bool readOnly;

  @override
  State<TrashPage> createState() => _TrashPageState();
}

class _TrashPageState extends State<TrashPage> {
  TrashList? _list;
  String? _error;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final list = await widget.api.trashList();
      if (mounted) {
        setState(() {
          _list = list;
          _error = null;
        });
      }
    } on Object catch (e) {
      if (mounted) {
        setState(() => _error = errorText(e));
      }
    }
  }

  Future<void> _restore(TrashItem item) async {
    String text;
    try {
      final path = await widget.api.restore(item.trashId);
      text = 'もとの場所へ戻した：$path';
    } on Object catch (e) {
      text = errorText(e);
    }
    await _load();
    if (mounted) {
      ScaffoldMessenger.of(context)
        ..hideCurrentSnackBar()
        ..showSnackBar(SnackBar(content: Text(text)));
    }
  }

  static String _reasonText(String reason) => switch (reason) {
    'delete' => '削除',
    'overwrite' => '上書き',
    'restore_replace' => '戻すときに置き換え',
    'orphan_tmp' => '書き込みの途中の残り',
    _ => reason,
  };

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('ゴミ箱')),
      body: _body(),
    );
  }

  Widget _body() {
    final error = _error;
    if (error != null) {
      return Center(child: Text(error, key: const Key('trash_error')));
    }
    final list = _list;
    if (list == null) {
      return const Center(child: CircularProgressIndicator());
    }
    final items = list.items;
    if (items.isEmpty) {
      return const Center(child: Text('ゴミ箱は空'));
    }
    final view = ListView.builder(
      itemCount: items.length,
      itemBuilder: (context, index) {
        final item = items[index];
        final hasPlace = item.originalPath.isNotEmpty;
        return ListTile(
          key: Key('trash:${item.trashId}'),
          leading: Icon(
            item.kind == 'dir'
                ? Icons.folder_outlined
                : Icons.description_outlined,
          ),
          title: Text(hasPlace ? item.originalPath : '（もとの場所なし）'),
          subtitle: Text('${_reasonText(item.reason)}・${item.deletedAt}'),
          trailing: TextButton(
            key: Key('restore:${item.trashId}'),
            onPressed: widget.readOnly || !hasPlace
                ? null
                : () => _restore(item),
            child: const Text('戻す'),
          ),
        );
      },
    );
    if (!list.truncated) {
      return view;
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const Padding(
          padding: EdgeInsets.all(8),
          child: Text(
            '一部だけ表示しています（多すぎるので、新しいものから順に途中まで）',
            key: Key('trash_partial_notice'),
          ),
        ),
        Expanded(child: view),
      ],
    );
  }
}
