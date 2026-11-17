import 'dart:async';

import 'package:flutter/material.dart';

import '../errors.dart';
import '../rescan.dart';
import '../vault_api.dart';
import 'file_page.dart';
import 'trash_page.dart';

/// 保管庫の中のフォルダの一覧。移動・表示・作成・削除（ゴミ箱へ）・ゴミ箱・更新。
///
/// 前面のあいだだけ、vault_changes（rescan:true）を呼んで、外での変更を見つける。間隔は 5 秒を基本に、
/// 前の走査にかかった時間に合わせて延ばす（rescan.dart）。「更新」のボタンでも呼ぶ。
class BrowserPage extends StatefulWidget {
  const BrowserPage({
    super.key,
    required this.api,
    this.autoRescan = true,
    this.rescanDelay = nextRescanDelay,
  });

  final VaultApi api;

  /// 前面のあいだ、変更の検知を自動で呼ぶか（テストでは止める）。
  final bool autoRescan;

  /// 前の走査にかかった時間から、次の走査までの間隔を決める関数（ふだんは [nextRescanDelay]）。
  final Duration Function(Duration lastScan) rescanDelay;

  @override
  State<BrowserPage> createState() => _BrowserPageState();
}

class _BrowserPageState extends State<BrowserPage> with WidgetsBindingObserver {
  VaultInfo? _info;
  String _dir = '';
  EntryList _list = const EntryList(entries: []);
  bool _loading = true;
  String? _error;

  int _seq = 0;
  bool _scanning = false;
  Duration _lastScan = Duration.zero;

  /// 自動の走査が続けて失敗した回数（失敗のたびに間隔を倍にする。成功で 0 に戻す）。
  int _failures = 0;
  Timer? _timer;
  bool _closed = false;

  /// 開いたあとの最初の読み込み（_start）が成功したか。成功するまでは、走査の予定を入れない
  /// （前面に戻ったときの予定も入れない）。
  bool _started = false;

  VaultApi get _api => widget.api;
  bool get _readOnly => _info?.readOnly ?? true;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    unawaited(_start());
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _timer?.cancel();
    if (!_closed) {
      _closed = true;
      unawaited(_api.close());
    }
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) {
      _schedule(Duration.zero);
    } else {
      // 前面でないあいだは、走査しない
      _timer?.cancel();
      _timer = null;
    }
  }

  bool get _inForeground {
    final state = WidgetsBinding.instance.lifecycleState;
    return state == null || state == AppLifecycleState.resumed;
  }

  Future<void> _start() async {
    try {
      final info = await _api.info();
      // 今の履歴の最後まで読み進めておく（開く前の変更は、一覧に出ているので数えない）
      var page = await _api.changes(sinceSeq: 0, rescan: false, limit: 10000);
      while (page.truncated) {
        page = await _api.changes(
          sinceSeq: page.nextSeq,
          rescan: false,
          limit: 10000,
        );
      }
      _seq = page.nextSeq;
      if (!mounted) {
        return;
      }
      _started = true;
      setState(() => _info = info);
      await _reload();
    } on Object catch (e) {
      if (mounted) {
        setState(() {
          _loading = false;
          _error = errorText(e);
        });
      }
      // 開けなかった：走査の予定は入れない
      return;
    }
    _schedule(baseRescanInterval);
  }

  void _schedule(Duration delay) {
    _timer?.cancel();
    _timer = null;
    if (!widget.autoRescan ||
        !_started ||
        !mounted ||
        _closed ||
        !_inForeground) {
      return;
    }
    _timer = Timer(delay, () => unawaited(_refresh(manual: false)));
  }

  /// 変更を調べ（rescan:true）、変わっていれば一覧を読み直す。
  Future<void> _refresh({required bool manual}) async {
    if (_scanning || _closed) {
      return;
    }
    _scanning = true;
    if (mounted) {
      setState(() {});
    }
    var failed = false;
    try {
      final watch = Stopwatch()..start();
      var page = await _api.changes(sinceSeq: _seq, rescan: true);
      watch.stop();
      if (page.scanned) {
        _lastScan = watch.elapsed;
      }
      var changed = page.count > 0;
      while (page.truncated) {
        page = await _api.changes(
          sinceSeq: page.nextSeq,
          rescan: false,
          limit: 10000,
        );
        changed = changed || page.count > 0;
      }
      _seq = page.nextSeq;
      if (changed || manual) {
        await _reload();
      }
    } on Object catch (e) {
      failed = true;
      if (manual) {
        _showMessage(errorText(e));
      } else if (_failures == 0) {
        // （「更新」のボタンの失敗は、その場で知らせるので、ここでは数えない）
        // 自動の走査の失敗は、続けて失敗しているあいだに一度だけ知らせる
        _showMessage('変更を調べられなかった（間隔を延ばして、また調べる）：${errorText(e)}');
      }
    } finally {
      // 続けて失敗した回数は、自動の走査だけで数える（成功すれば、どちらでも 0 に戻す）
      if (!failed) {
        _failures = 0;
      } else if (!manual) {
        _failures += 1;
      }
      _scanning = false;
      if (mounted) {
        setState(() {});
      }
      _schedule(backoffRescanDelay(widget.rescanDelay(_lastScan), _failures));
    }
  }

  Future<void> _reload() async {
    try {
      final list = await _api.list(_dir);
      if (mounted) {
        setState(() {
          _list = list;
          _loading = false;
          _error = null;
        });
      }
    } on Object catch (e) {
      if (mounted) {
        setState(() {
          _loading = false;
          _error = errorText(e);
        });
      }
    }
  }

  void _showMessage(String text) {
    if (!mounted) {
      return;
    }
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(text)));
  }

  /// 保管庫の中の相対パスをつなぐ（区切りは /。名前の確かめは vault-core が行う）。
  String _join(String name) => _dir.isEmpty ? name : '$_dir/$name';

  Future<void> _go(String dir) async {
    setState(() {
      _dir = dir;
      _loading = true;
    });
    await _reload();
  }

  Future<void> _up() async {
    final cut = _dir.lastIndexOf('/');
    await _go(cut < 0 ? '' : _dir.substring(0, cut));
  }

  Future<void> _openEntry(VaultEntry entry) async {
    if (entry.isDir) {
      await _go(entry.path);
      return;
    }
    await Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => FilePage(api: _api, path: entry.path),
      ),
    );
  }

  Future<void> _newFolder() async {
    final name = await _askText(
      title: '新しいフォルダ',
      label: 'フォルダの名前',
      fieldKey: const Key('new_folder_name'),
    );
    if (name == null || name.isEmpty) {
      return;
    }
    await _act(() => _api.createFolder(_join(name)), done: 'フォルダを作った');
  }

  Future<void> _newFile() async {
    final result = await showDialog<(String, String)>(
      context: context,
      builder: (context) => const _NewFileDialog(),
    );
    if (result == null || result.$1.isEmpty) {
      return;
    }
    await _act(
      () => _api.createFile(_join(result.$1), result.$2),
      done: 'ファイルを作った',
    );
  }

  Future<void> _delete(VaultEntry entry) async {
    final what = entry.isDir ? 'フォルダ「${entry.name}」を中身ごと' : '「${entry.name}」を';
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('ゴミ箱へ移す'),
        content: Text('$whatゴミ箱へ移す。ゴミ箱から戻せる。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('やめる'),
          ),
          FilledButton(
            key: const Key('confirm_delete'),
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('ゴミ箱へ移す'),
          ),
        ],
      ),
    );
    if (ok != true) {
      return;
    }
    await _act(
      () => _api.delete(entry.path, recursive: entry.isDir),
      done: 'ゴミ箱へ移した（ゴミ箱から戻せる）',
    );
  }

  Future<void> _openTrash() async {
    await Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => TrashPage(api: _api, readOnly: _readOnly),
      ),
    );
    await _reload();
  }

  Future<void> _closeVault() async {
    _timer?.cancel();
    if (!_closed) {
      _closed = true;
      await _api.close();
    }
    if (mounted) {
      Navigator.of(context).pop();
    }
  }

  /// 操作をして、成功なら一覧を読み直して知らせ、失敗なら決まった文で知らせる。
  Future<void> _act(
    Future<Object?> Function() action, {
    required String done,
  }) async {
    try {
      await action();
      await _reload();
      _showMessage(done);
    } on Object catch (e) {
      _showMessage(errorText(e));
    }
  }

  Future<String?> _askText({
    required String title,
    required String label,
    required Key fieldKey,
  }) {
    return showDialog<String>(
      context: context,
      builder: (context) =>
          _TextDialog(title: title, label: label, fieldKey: fieldKey),
    );
  }

  @override
  Widget build(BuildContext context) {
    final info = _info;
    return Scaffold(
      appBar: AppBar(
        title: Text(
          info == null
              ? '保管庫'
              : '保管庫：${info.name}${info.readOnly ? '（読み取り専用）' : ''}',
        ),
        actions: [
          IconButton(
            key: const Key('refresh'),
            tooltip: '更新（ほかのアプリでの変更を調べる）',
            onPressed: _scanning ? null : () => _refresh(manual: true),
            icon: const Icon(Icons.refresh),
          ),
          IconButton(
            key: const Key('trash'),
            tooltip: 'ゴミ箱',
            onPressed: _openTrash,
            icon: const Icon(Icons.delete_outline),
          ),
          IconButton(
            key: const Key('close'),
            tooltip: '保管庫を閉じる',
            onPressed: _closeVault,
            icon: const Icon(Icons.close),
          ),
        ],
      ),
      floatingActionButton: _readOnly
          ? null
          : Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                FloatingActionButton.small(
                  key: const Key('new_folder'),
                  heroTag: 'new_folder',
                  tooltip: '新しいフォルダ',
                  onPressed: _newFolder,
                  child: const Icon(Icons.create_new_folder_outlined),
                ),
                const SizedBox(height: 8),
                FloatingActionButton.small(
                  key: const Key('new_file'),
                  heroTag: 'new_file',
                  tooltip: '新しいファイル',
                  onPressed: _newFile,
                  child: const Icon(Icons.note_add_outlined),
                ),
              ],
            ),
      body: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          ListTile(
            leading: IconButton(
              key: const Key('up'),
              tooltip: '上のフォルダへ',
              onPressed: _dir.isEmpty ? null : _up,
              icon: const Icon(Icons.arrow_upward),
            ),
            title: Text(
              _dir.isEmpty ? '（保管庫の根）' : _dir,
              key: const Key('current_dir'),
            ),
            trailing: _scanning
                ? const SizedBox.square(
                    dimension: 16,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : null,
          ),
          const Divider(height: 1),
          if (!_loading && _error == null && _list.partial)
            Padding(
              padding: const EdgeInsets.all(8),
              child: Text(
                partialListText(_list),
                key: const Key('partial_notice'),
              ),
            ),
          Expanded(child: _body()),
        ],
      ),
    );
  }

  Widget _body() {
    if (_loading) {
      return const Center(child: CircularProgressIndicator());
    }
    final error = _error;
    if (error != null) {
      return Center(child: Text(error, key: const Key('browser_error')));
    }
    final entries = _list.entries;
    if (entries.isEmpty) {
      return const Center(child: Text('空のフォルダ'));
    }
    return ListView.builder(
      itemCount: entries.length,
      itemBuilder: (context, index) {
        final entry = entries[index];
        return ListTile(
          key: Key('entry:${entry.path}'),
          leading: Icon(switch (entry.kind) {
            'dir' => Icons.folder_outlined,
            'link' => Icons.link,
            _ => Icons.description_outlined,
          }),
          title: Text(entry.name),
          subtitle: Text(
            entry.isDir
                ? entry.modified
                : '${entry.size} バイト・${entry.modified}',
          ),
          onTap: () => _openEntry(entry),
          trailing: _readOnly
              ? null
              : IconButton(
                  key: Key('delete:${entry.path}'),
                  tooltip: 'ゴミ箱へ移す',
                  onPressed: () => _delete(entry),
                  icon: const Icon(Icons.delete_outline),
                ),
        );
      },
    );
  }
}

/// 名前を1つ入力するダイアログ。
class _TextDialog extends StatefulWidget {
  const _TextDialog({
    required this.title,
    required this.label,
    required this.fieldKey,
  });

  final String title;
  final String label;
  final Key fieldKey;

  @override
  State<_TextDialog> createState() => _TextDialogState();
}

class _TextDialogState extends State<_TextDialog> {
  final TextEditingController _text = TextEditingController();

  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: Text(widget.title),
      content: TextField(
        key: widget.fieldKey,
        controller: _text,
        autofocus: true,
        decoration: InputDecoration(labelText: widget.label),
        onSubmitted: (value) => Navigator.of(context).pop(value),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('やめる'),
        ),
        FilledButton(
          key: const Key('dialog_ok'),
          onPressed: () => Navigator.of(context).pop(_text.text),
          child: const Text('作る'),
        ),
      ],
    );
  }
}

/// 新しいファイルの名前と中身を入力するダイアログ。
class _NewFileDialog extends StatefulWidget {
  const _NewFileDialog();

  @override
  State<_NewFileDialog> createState() => _NewFileDialogState();
}

class _NewFileDialogState extends State<_NewFileDialog> {
  final TextEditingController _name = TextEditingController();
  final TextEditingController _content = TextEditingController();

  @override
  void dispose() {
    _name.dispose();
    _content.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('新しいファイル'),
      content: SizedBox(
        width: 480,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            TextField(
              key: const Key('new_file_name'),
              controller: _name,
              autofocus: true,
              decoration: const InputDecoration(
                labelText: 'ファイルの名前（例：memo.txt）',
              ),
            ),
            const SizedBox(height: 8),
            TextField(
              key: const Key('new_file_content'),
              controller: _content,
              minLines: 4,
              maxLines: 10,
              decoration: const InputDecoration(
                labelText: '中身（文字）',
                border: OutlineInputBorder(),
              ),
            ),
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('やめる'),
        ),
        FilledButton(
          key: const Key('dialog_ok'),
          onPressed: () =>
              Navigator.of(context).pop((_name.text, _content.text)),
          child: const Text('作る'),
        ),
      ],
    );
  }
}

/// 一覧が一部だけのときの決まった文。
/// 名前そのものは文に入れない（扱えない名前は、数だけを出す）。
String partialListText(EntryList list) {
  final parts = <String>[];
  if (list.truncated) {
    parts.add('多すぎるので、はじめの部分だけ');
  }
  if (list.skippedOverLimit > 0) {
    parts.add('深すぎる・長すぎる場所のもの（${list.skippedOverLimit} 件）を除いて');
  }
  if (list.skippedInvalidNames > 0) {
    parts.add('保管庫で扱えない名前のもの（${list.skippedInvalidNames} 件）を除いて');
  }
  return '一部だけ表示しています（${parts.join('、')}）';
}
