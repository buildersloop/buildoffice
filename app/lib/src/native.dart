/// vault-ffi（Rust の C ABI）を、dart:ffi だけで呼ぶ。
///
/// - DLL は、呼ぶ側が渡す絶対パスで読み込む（アプリは exe のフォルダの vault_ffi.dll）。
/// - vault-ffi が返すバッファと、vault-ffi で確保したバッファは、[NativeBuffer] で包む。生のポインタは、このファイルの
///   外に出さない（`_` で始まる名前は、このファイルの中だけで使える）。
/// - どのバッファも try/finally で1回だけ vf_free する（[NativeBuffer.free] は、2回目からは何もしない）。
/// - out_len も vf_alloc で取り、0 を入れてから渡す（package:ffi の calloc は使わない）。要求のバッファも vf_alloc で取る。
/// - このファイルの関数は、呼んだスレッドで止まって待つ。アプリは [Isolate.run] の中で呼ぶ（vault_client.dart）。
library;

import 'dart:convert';
import 'dart:ffi';
import 'dart:typed_data';

import 'package:flutter/foundation.dart' show visibleForTesting;

import 'errors.dart';

typedef _AllocNative = Pointer<Uint8> Function(Size len);
typedef _AllocDart = Pointer<Uint8> Function(int len);
typedef _FreeNative = Void Function(Pointer<Uint8> ptr, Size len);
typedef _FreeDart = void Function(Pointer<Uint8> ptr, int len);
typedef _OpenNative =
    Pointer<Uint8> Function(Pointer<Uint8> req, Size len, Pointer<Size> outLen);
typedef _OpenDart =
    Pointer<Uint8> Function(Pointer<Uint8> req, int len, Pointer<Size> outLen);
typedef _CallNative =
    Pointer<Uint8> Function(
      Uint64 handle,
      Pointer<Uint8> req,
      Size len,
      Pointer<Size> outLen,
    );
typedef _CallDart =
    Pointer<Uint8> Function(
      int handle,
      Pointer<Uint8> req,
      int len,
      Pointer<Size> outLen,
    );
typedef _CloseNative = Void Function(Uint64 handle);
typedef _CloseDart = void Function(int handle);

/// 要求を送る関数（vf_open か vf_call）。
typedef _Send =
    Pointer<Uint8> Function(Pointer<Uint8> req, int len, Pointer<Size> outLen);

/// 読み込んだ vault_ffi.dll。要求と返り値は、JSON の文字で受け渡す。
final class VaultLibrary {
  VaultLibrary._(DynamicLibrary lib)
    : _alloc = lib.lookupFunction<_AllocNative, _AllocDart>('vf_alloc'),
      _free = lib.lookupFunction<_FreeNative, _FreeDart>('vf_free'),
      _open = lib.lookupFunction<_OpenNative, _OpenDart>('vf_open'),
      _call = lib.lookupFunction<_CallNative, _CallDart>('vf_call'),
      _close = lib.lookupFunction<_CloseNative, _CloseDart>('vf_close');

  /// `dllPath`（絶対パス）の vault_ffi.dll を読み込む。
  factory VaultLibrary.open(String dllPath) =>
      VaultLibrary._(DynamicLibrary.open(dllPath));

  final _AllocDart _alloc;
  final _FreeDart _free;
  final _OpenDart _open;
  final _CallDart _call;
  final _CloseDart _close;

  /// vf_open（`{"root","init","read_only"}`）。返り値は `{"ok":…}` か `{"error":…}` の JSON の文字。
  String open(String requestJson) => _exchange(requestJson, _open);

  /// vf_call（`{"tool","args"}`）。
  String call(int handle, String requestJson) => _exchange(
    requestJson,
    (req, len, outLen) => _call(handle, req, len, outLen),
  );

  /// vf_close（返り値はない。知らない・閉じたハンドルなら、vault-ffi は何もしない）。
  void close(int handle) => _close(handle);

  /// テストだけ：vault-ffi でバッファを確保する（解放は [NativeBuffer.free]）。
  @visibleForTesting
  NativeBuffer allocateForTest(int length) => _allocate(length);

  /// 要求を書いて送り、返り値を文字にして返す。要求・out_len・返り値のバッファは、finally で1回だけ解放する。
  String _exchange(String requestJson, _Send send) {
    final bytes = utf8.encode(requestJson);
    final request = _allocate(bytes.length);
    NativeBuffer? outLen;
    NativeBuffer? reply;
    try {
      request._write(bytes);
      outLen = _allocate(sizeOf<Size>());
      final outLenPointer = outLen._pointer.cast<Size>();
      // vault-ffi の決まり（# Safety）：out_len は usize の並びにそろった場所
      if (outLenPointer.address % sizeOf<Size>() != 0) {
        throw const VaultException('INTERNAL', 'out_len の場所がそろっていない');
      }
      outLenPointer.value = 0;
      final pointer = send(request._pointer, bytes.length, outLenPointer);
      final length = outLenPointer.value;
      if (pointer == nullptr) {
        throw const VaultException('INTERNAL', 'vault-ffi が返り値を作れなかった');
      }
      reply = NativeBuffer._(this, pointer, length);
      return utf8.decode(reply._read());
    } finally {
      reply?.free();
      outLen?.free();
      request.free();
    }
  }

  /// vault-ffi でバッファを確保する（0 か 32 MiB を超えるときは、vault-ffi が null を返す）。
  NativeBuffer _allocate(int length) {
    final pointer = _alloc(length);
    if (pointer == nullptr) {
      throw const VaultException('INVALID_ARGUMENT', '要求が大きすぎるか、空（32 MiB まで）');
    }
    return NativeBuffer._(this, pointer, length);
  }
}

/// vault-ffi が確保したバッファ。生のポインタは、このファイルの外に出さない。解放は1回だけ。
final class NativeBuffer {
  NativeBuffer._(this._library, Pointer<Uint8> pointer, this.length)
    : _pointerOrNull = pointer;

  final VaultLibrary _library;

  /// 長さ（vf_free に、確保したときと同じ長さで渡す）。
  final int length;

  Pointer<Uint8>? _pointerOrNull;

  /// もう解放したか。
  bool get isFreed => _pointerOrNull == null;

  Pointer<Uint8> get _pointer {
    final pointer = _pointerOrNull;
    if (pointer == null) {
      throw StateError('解放したバッファは使えない');
    }
    return pointer;
  }

  void _write(List<int> bytes) {
    _pointer.asTypedList(length).setRange(0, bytes.length, bytes);
  }

  Uint8List _read() => Uint8List.fromList(_pointer.asTypedList(length));

  /// 解放する（vf_free）。2回目からは何もしない。
  void free() {
    final pointer = _pointerOrNull;
    if (pointer == null) {
      return;
    }
    _pointerOrNull = null;
    _library._free(pointer, length);
  }
}
