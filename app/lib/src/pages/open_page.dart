import 'package:flutter/material.dart';

import '../errors.dart';
import '../folder_picker.dart';
import '../vault_api.dart';
import 'browser_page.dart';

/// 既定の保管庫のカードの説明（保管庫とは何か、起動のたびに開くこと、消すと作り直すこと。パスは書かない）。
const String defaultCardText =
    '保管庫は、ファイルを入れておくアプリの専用のフォルダ（消したものはゴミ箱から戻せる）。'
    '既定の保管庫は、ホームのフォルダの中の $defaultVaultName で、起動のたびに開く（なければ作る）。'
    '消しても次の起動で作り直す。';

/// 既定の保管庫を作ったときの知らせ。
const String defaultCreatedText = '保管庫を作った（ホームのフォルダの中の $defaultVaultName）';

/// 既定の保管庫を開けなかったときの文の書き出し。
const String defaultErrorHead = '既定の保管庫を開けなかった：';

/// 既定の場所が、保管庫でないフォルダだったとき（直し方まで言う）。
const String defaultNotAVaultText =
    'ホームのフォルダの中の $defaultVaultName は保管庫ではない。'
    '名前を変えるか中身をほかへ移してから、もう一度押す';

/// 既定の場所を保管庫にできないとき（リンク・ファイル・ホームが見つからない など）。
const String defaultInvalidRootText =
    'ホームのフォルダの中の $defaultVaultName を保管庫として使えない'
    '（リンクやファイルになっている など）。名前を変えるか除けてから、もう一度押す';

/// 既定の保管庫を開けなかったときに、カードに出す文。決まった文だけで、絶対パスは出さない（vault-core の説明の文は
/// パスを含まないので、INVALID_ROOT では添える）。vault-core のエラーでない例外は、決まった文だけ。
String defaultErrorText(Object error) {
  if (error is VaultException) {
    switch (error.code) {
      case 'NOT_A_VAULT':
        return '$defaultErrorHead$defaultNotAVaultText';
      case 'INVALID_ROOT':
        return error.message.isEmpty
            ? '$defaultErrorHead$defaultInvalidRootText'
            : '$defaultErrorHead$defaultInvalidRootText：${error.message}';
    }
  }
  return '$defaultErrorHead${errorText(error)}';
}

/// 保管庫を開く・新しく作る画面。いちばん上のカードで既定の保管庫（ホームのフォルダの中の Buildoffice）を開き、
/// 次のカードで、すでにある保管庫を開き、下のカードで、置き場所の中に名前を付けて新しく作る。パスは覚えない
/// （設定のファイルを作らない）。既定の保管庫の場所を決めて作る・開くのも、パスと名前の確かめも vault-core が行い、
/// ここでは前後の空白を除くだけ。画面にパスが出るのは、利用者が入れた欄の中だけ。
class OpenPage extends StatefulWidget {
  const OpenPage({
    super.key,
    required this.opener,
    required this.creator,
    required this.defaultOpener,
    this.openDefaultOnStart = false,
    this.picker = const ChannelFolderPicker(),
    this.autoRescan = true,
  });

  /// 保管庫を開く関数（本物は vault-ffi）。
  final VaultOpener opener;

  /// 保管庫を作って開く関数（本物は vault-ffi）。
  final VaultCreator creator;

  /// 既定の保管庫を開く関数（なければ作る）。既定の値は置かない（本物を渡すのは VaultApp だけ）。
  final VaultDefaultOpener defaultOpener;

  /// 画面が最初にできたときに1回だけ、既定の保管庫を開くか（アプリの起動）。既定は false。閉じて戻ったときは
  /// 開き直さない（「既定の保管庫を開く」のボタンで開く）。
  final bool openDefaultOnStart;

  /// フォルダを選ぶ窓（本物は runner のチャンネル）。
  final FolderPicker picker;

  /// 開いたあと、前面のあいだ変更の検知を自動で呼ぶか（テストでは止める）。
  final bool autoRescan;

  @override
  State<OpenPage> createState() => _OpenPageState();
}

class _OpenPageState extends State<OpenPage> {
  final TextEditingController _root = TextEditingController();
  final TextEditingController _parent = TextEditingController();
  final TextEditingController _name = TextEditingController();
  bool _readOnly = false;

  /// 「開く」「新しく作る」「選ぶ…」の処理中（そのあいだは、どのボタンも押せない）。
  bool _busy = false;

  /// 既定の保管庫を開いている最中（そのあいだは「既定の保管庫を開く」だけを押せない。ほかのカードは使える）。
  bool _defaultBusy = false;

  /// 一覧の画面を出しているか（出しているあいだに返ってきた保管庫は、閉じる）。
  bool _showing = false;

  /// 一覧の画面へ移った回数（既定の保管庫が遅れて返ったとき、そのあいだにほかの保管庫を開いていれば閉じる）。
  int _shownCount = 0;

  String? _defaultError;
  String? _openError;
  String? _createError;

  @override
  void initState() {
    super.initState();
    if (widget.openDefaultOnStart) {
      // 最初のフレームから「既定の保管庫を開く」を押せないようにし、フレームのあとで1回だけ呼ぶ。起動の1回は
      // _run を通さない（呼び出しのあいだも「開く」「新しく作る」は使える）
      _defaultBusy = true;
      WidgetsBinding.instance.addPostFrameCallback(
        (_) => _openDefault(onStart: true),
      );
    }
  }

  @override
  void dispose() {
    _root.dispose();
    _parent.dispose();
    _name.dispose();
    super.dispose();
  }

  /// 処理を1つだけ動かす（処理中はどのボタンも押せない）。
  Future<void> _run(Future<void> Function() action) async {
    if (_busy) {
      return;
    }
    setState(() {
      _busy = true;
      _openError = null;
      _createError = null;
    });
    try {
      await action();
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  /// 既定の保管庫を開く（なければ作る）。起動の1回（[onStart]）と、ボタンから。
  Future<void> _openDefault({bool onStart = false}) async {
    if (!mounted || (!onStart && (_defaultBusy || _busy))) {
      return;
    }
    setState(() {
      _defaultBusy = true;
      _defaultError = null;
    });
    final shownBefore = _shownCount;
    try {
      final opened = await widget.defaultOpener();
      if (!mounted || _shownCount != shownBefore) {
        // 画面が消えた・待っているあいだにほかの保管庫を開いた：遅れて返った保管庫は閉じる（作ったものは消さない）
        await opened.api.close();
        return;
      }
      if (opened.created) {
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(const SnackBar(content: Text(defaultCreatedText)));
      }
      await _show(opened.api);
    } on Object catch (e) {
      if (mounted) {
        setState(() => _defaultError = defaultErrorText(e));
      }
    } finally {
      if (mounted) {
        setState(() => _defaultBusy = false);
      }
    }
  }

  Future<void> _open() => _run(() async {
    try {
      final api = await widget.opener(
        root: _root.text.trim(),
        init: false,
        readOnly: _readOnly,
      );
      await _show(api);
    } on Object catch (e) {
      if (mounted) {
        setState(() => _openError = errorText(e));
      }
    }
  });

  Future<void> _create() => _run(() async {
    try {
      final api = await widget.creator(
        parent: _parent.text.trim(),
        name: _name.text.trim(),
      );
      if (mounted) {
        ScaffoldMessenger.of(
          context,
        ).showSnackBar(const SnackBar(content: Text('保管庫を作った')));
      }
      await _show(api);
    } on Object catch (e) {
      if (mounted) {
        setState(() => _createError = errorText(e));
      }
    }
  });

  /// 開いた保管庫の一覧の画面へ移る。画面がもうないとき・ほかの保管庫の一覧を出しているときは、閉じる。
  Future<void> _show(VaultApi api) async {
    if (!mounted || _showing) {
      await api.close();
      return;
    }
    _showing = true;
    _shownCount++;
    try {
      await Navigator.of(context).push(
        MaterialPageRoute<void>(
          builder: (_) => BrowserPage(api: api, autoRescan: widget.autoRescan),
        ),
      );
    } finally {
      _showing = false;
    }
  }

  /// フォルダを選ぶ窓を出し、選んだパスを欄に入れる（取り消したら変えない）。
  Future<void> _pick(TextEditingController field, {required bool forCreate}) =>
      _run(() async {
        try {
          final picked = await widget.picker.pickFolder();
          if (picked != null) {
            field.text = picked;
          }
        } on FolderPickerException catch (e) {
          _setError(e.message, forCreate: forCreate);
        } on Object {
          _setError(folderPickerFailedText, forCreate: forCreate);
        }
      });

  void _setError(String text, {required bool forCreate}) {
    if (!mounted) {
      return;
    }
    setState(() {
      if (forCreate) {
        _createError = text;
      } else {
        _openError = text;
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(title: const Text('保管庫を開く・作る')),
      body: SingleChildScrollView(
        padding: const EdgeInsets.all(24),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _card(
              key: const Key('default_card'),
              title: '既定の保管庫',
              children: [
                const Text(defaultCardText, key: Key('default_note')),
                const SizedBox(height: 12),
                Align(
                  alignment: Alignment.centerLeft,
                  child: FilledButton(
                    key: const Key('open_default'),
                    onPressed: _busy || _defaultBusy
                        ? null
                        : () => _openDefault(),
                    child: const Text('既定の保管庫を開く'),
                  ),
                ),
                if (_defaultBusy)
                  const Padding(
                    padding: EdgeInsets.only(top: 12),
                    child: Text('既定の保管庫を開いている…', key: Key('default_busy')),
                  ),
                if (_defaultError != null)
                  _errorText(const Key('default_error'), _defaultError!, theme),
              ],
            ),
            const SizedBox(height: 16),
            _card(
              key: const Key('open_card'),
              title: '保管庫を開く',
              children: [
                const Text('すでにある保管庫のフォルダを選ぶか、絶対パスを入力する。パスは覚えないので、開くたびに選ぶ。'),
                const SizedBox(height: 12),
                _folderField(
                  fieldKey: const Key('root'),
                  pickKey: const Key('pick_root'),
                  controller: _root,
                  label: '保管庫のフォルダ',
                  onPick: () => _pick(_root, forCreate: false),
                  onSubmitted: _open,
                ),
                CheckboxListTile(
                  key: const Key('read_only'),
                  contentPadding: EdgeInsets.zero,
                  title: const Text('読み取り専用で開く'),
                  value: _readOnly,
                  onChanged: _busy
                      ? null
                      : (value) => setState(() => _readOnly = value ?? false),
                ),
                Align(
                  alignment: Alignment.centerLeft,
                  child: FilledButton(
                    key: const Key('open'),
                    onPressed: _busy ? null : _open,
                    child: const Text('開く'),
                  ),
                ),
                if (_openError != null)
                  _errorText(const Key('open_error'), _openError!, theme),
              ],
            ),
            const SizedBox(height: 16),
            _card(
              key: const Key('create_card'),
              title: '新しく作る',
              children: [
                const Text(
                  'すでにあるフォルダ（置き場所）の中に、名前を付けて新しい保管庫のフォルダを作り、そのまま開く。'
                  '置き場所のフォルダは作らない。',
                ),
                const SizedBox(height: 12),
                _folderField(
                  fieldKey: const Key('parent'),
                  pickKey: const Key('pick_parent'),
                  controller: _parent,
                  label: '置き場所',
                  onPick: () => _pick(_parent, forCreate: true),
                  onSubmitted: _create,
                ),
                const SizedBox(height: 12),
                TextField(
                  key: const Key('name'),
                  controller: _name,
                  enabled: !_busy,
                  decoration: const InputDecoration(
                    labelText: '保管庫の名前',
                    border: OutlineInputBorder(),
                  ),
                  onSubmitted: (_) => _busy ? null : _create(),
                ),
                const SizedBox(height: 8),
                Text(
                  '作れない場所：ドライブの根、ホームのフォルダそのもの、AppData・Windows・Program Files・ProgramData の下、'
                  'ネットワークの場所、ほかの保管庫の中。中身のあるフォルダも保管庫にできない。',
                  key: const Key('create_note'),
                  style: theme.textTheme.bodySmall,
                ),
                const SizedBox(height: 12),
                Align(
                  alignment: Alignment.centerLeft,
                  child: FilledButton(
                    key: const Key('create'),
                    onPressed: _busy ? null : _create,
                    child: const Text('作って開く'),
                  ),
                ),
                if (_createError != null)
                  _errorText(const Key('create_error'), _createError!, theme),
              ],
            ),
          ],
        ),
      ),
    );
  }

  Widget _card({
    required Key key,
    required String title,
    required List<Widget> children,
  }) {
    return Card(
      key: key,
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(title, style: Theme.of(context).textTheme.titleMedium),
            const SizedBox(height: 8),
            ...children,
          ],
        ),
      ),
    );
  }

  Widget _folderField({
    required Key fieldKey,
    required Key pickKey,
    required TextEditingController controller,
    required String label,
    required Future<void> Function() onPick,
    required Future<void> Function() onSubmitted,
  }) {
    return Row(
      children: [
        Expanded(
          child: TextField(
            key: fieldKey,
            controller: controller,
            enabled: !_busy,
            decoration: InputDecoration(
              labelText: label,
              border: const OutlineInputBorder(),
            ),
            onSubmitted: (_) => _busy ? null : onSubmitted(),
          ),
        ),
        const SizedBox(width: 8),
        OutlinedButton(
          key: pickKey,
          onPressed: _busy ? null : onPick,
          child: const Text('選ぶ…'),
        ),
      ],
    );
  }

  Widget _errorText(Key key, String text, ThemeData theme) {
    return Padding(
      padding: const EdgeInsets.only(top: 12),
      child: Text(
        text,
        key: key,
        style: TextStyle(color: theme.colorScheme.error),
      ),
    );
  }
}
