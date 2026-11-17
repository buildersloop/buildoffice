import 'package:flutter/material.dart';

import '../errors.dart';
import '../vault_api.dart';
import 'browser_page.dart';

/// 保管庫のパスを入力して開く画面。パスは覚えない（設定のファイルを作らない）。
class OpenPage extends StatefulWidget {
  const OpenPage({super.key, required this.opener, this.autoRescan = true});

  /// 保管庫を開く関数（本物は vault-ffi）。
  final VaultOpener opener;

  /// 開いたあと、前面のあいだ変更の検知を自動で呼ぶか（テストでは止める）。
  final bool autoRescan;

  @override
  State<OpenPage> createState() => _OpenPageState();
}

class _OpenPageState extends State<OpenPage> {
  final TextEditingController _root = TextEditingController();
  bool _create = false;
  bool _readOnly = false;
  bool _busy = false;
  String? _error;

  @override
  void dispose() {
    _root.dispose();
    super.dispose();
  }

  Future<void> _open() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      // パスの確かめは vault-core が行う（Dart では作り直さない）。前後の空白だけを除く
      final api = await widget.opener(
        root: _root.text.trim(),
        init: _create,
        readOnly: _readOnly,
      );
      if (!mounted) {
        await api.close();
        return;
      }
      await Navigator.of(context).push(
        MaterialPageRoute<void>(
          builder: (_) => BrowserPage(api: api, autoRescan: widget.autoRescan),
        ),
      );
    } on Object catch (e) {
      if (mounted) {
        setState(() => _error = errorText(e));
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('保管庫を開く')),
      body: Padding(
        padding: const EdgeInsets.all(24),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text(
              '保管庫のフォルダの絶対パスを入力する（例：C:\\Vaults\\MyVault）。'
              'パスは覚えないので、開くたびに入力する。',
            ),
            const SizedBox(height: 16),
            TextField(
              key: const Key('root'),
              controller: _root,
              decoration: const InputDecoration(
                labelText: '保管庫のパス',
                border: OutlineInputBorder(),
              ),
              onSubmitted: (_) => _busy ? null : _open(),
            ),
            const SizedBox(height: 16),
            SegmentedButton<bool>(
              segments: const [
                ButtonSegment(value: false, label: Text('開く')),
                ButtonSegment(value: true, label: Text('作って開く')),
              ],
              selected: {_create},
              onSelectionChanged: (selected) =>
                  setState(() => _create = selected.first),
            ),
            CheckboxListTile(
              key: const Key('read_only'),
              contentPadding: EdgeInsets.zero,
              title: const Text('読み取り専用で開く'),
              value: _readOnly,
              onChanged: (value) => setState(() => _readOnly = value ?? false),
            ),
            const SizedBox(height: 8),
            FilledButton(
              key: const Key('open'),
              onPressed: _busy ? null : _open,
              child: Text(_create ? '作って開く' : '開く'),
            ),
            if (_error != null) ...[
              const SizedBox(height: 16),
              Text(
                _error!,
                key: const Key('open_error'),
                style: TextStyle(color: Theme.of(context).colorScheme.error),
              ),
            ],
          ],
        ),
      ),
    );
  }
}
