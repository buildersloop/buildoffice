/// フォルダを選ぶ窓（OS の標準の窓）。窓を出すのは runner（app/windows/runner の flutter_window.cpp）で、ここは
/// チャンネルで頼んで、選ばれたパスを受け取るだけ。パスの確かめは vault-core が行う（ここでは確かめない）。
library;

import 'package:flutter/services.dart';

/// チャンネルの名前（runner の flutter_window.cpp と同じ）。
const String folderPickerChannelName = 'buildoffice/folder_picker';

/// 窓を出せないとき（この環境に runner の受け口がない）の決まった文。
const String folderPickerUnavailableText = 'フォルダを選ぶ窓を出せない（パスを入力する）';

/// 窓で選べなかったときの決まった文（OS の文やパスは出さない）。
const String folderPickerFailedText = 'フォルダを選べなかった（もう一度選ぶか、パスを入力する）';

/// フォルダを選べなかったときの例外。[message] は決まった文だけ。
class FolderPickerException implements Exception {
  const FolderPickerException(this.message);

  final String message;

  @override
  String toString() => message;
}

/// フォルダを選ぶ（テストでは偽のものに差し替える）。
abstract interface class FolderPicker {
  /// 選んだフォルダの絶対パス。取り消したら null。選べなければ [FolderPickerException]。
  Future<String?> pickFolder();
}

/// runner のチャンネル（`buildoffice/folder_picker` の `pickFolder`）で、OS の窓を出す。
final class ChannelFolderPicker implements FolderPicker {
  const ChannelFolderPicker({
    this.channel = const MethodChannel(folderPickerChannelName),
  });

  final MethodChannel channel;

  @override
  Future<String?> pickFolder() async {
    final Object? picked;
    try {
      picked = await channel.invokeMethod<Object?>('pickFolder');
    } on MissingPluginException {
      throw const FolderPickerException(folderPickerUnavailableText);
    } on Object {
      // PlatformException など。元の文は出さない
      throw const FolderPickerException(folderPickerFailedText);
    }
    if (picked == null) {
      return null;
    }
    if (picked is! String || picked.isEmpty) {
      throw const FolderPickerException(folderPickerFailedText);
    }
    return picked;
  }
}
