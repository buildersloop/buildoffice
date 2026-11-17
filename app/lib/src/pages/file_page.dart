import 'package:flutter/material.dart';

import '../errors.dart';
import '../vault_api.dart';

/// ファイルの中身（文字）を表示する画面。編集はしない（Phase 0）。
class FilePage extends StatefulWidget {
  const FilePage({super.key, required this.api, required this.path});

  final VaultApi api;
  final String path;

  @override
  State<FilePage> createState() => _FilePageState();
}

class _FilePageState extends State<FilePage> {
  late final Future<FileText> _text = widget.api.readText(widget.path);

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: Text(widget.path)),
      body: FutureBuilder<FileText>(
        future: _text,
        builder: (context, snapshot) {
          final error = snapshot.error;
          if (error != null) {
            return Center(
              child: Text(errorText(error), key: const Key('file_error')),
            );
          }
          final text = snapshot.data;
          if (text == null) {
            return const Center(child: CircularProgressIndicator());
          }
          return SingleChildScrollView(
            padding: const EdgeInsets.all(16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                if (text.truncated)
                  const Padding(
                    padding: EdgeInsets.only(bottom: 8),
                    child: Text('長いので、はじめの部分だけを表示している'),
                  ),
                SelectableText(text.content, key: const Key('file_content')),
              ],
            ),
          );
        },
      ),
    );
  }
}
