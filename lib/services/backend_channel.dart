import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:isolate';

import 'package:ffi/ffi.dart';
import 'package:flutter/foundation.dart' show optionalTypeArgs;
import 'package:flutter/services.dart';

/// The `com.zarz.mornye/backend` channel. Android and iOS answer it from
/// Kotlin and Swift; Windows has no such host, so calls go straight to the
/// Rust backend (`mornye_mobile.dll`, feature `desktop-host`) on a worker
/// isolate. Replies keep the platform channel's shape: the same values, and
/// failures as [PlatformException] with code `ERROR`.
class BackendChannel {
  const BackendChannel(this.name);

  final String name;

  /// Under `flutter test` the channel stays a [MethodChannel] on every host,
  /// so tests can mock it as they do for Android and iOS.
  static final bool _usesDesktopBackend =
      Platform.isWindows && !Platform.environment.containsKey('FLUTTER_TEST');

  MethodChannel get _platform => MethodChannel(name);

  @optionalTypeArgs
  Future<T?> invokeMethod<T>(String method, [dynamic arguments]) async {
    if (!_usesDesktopBackend) {
      return _platform.invokeMethod<T>(method, arguments);
    }
    final encoded = jsonEncode(arguments ?? const <String, Object?>{});
    final reply = await Isolate.run(() => _invokeDesktop(method, encoded));
    final envelope = jsonDecode(reply) as Map<String, dynamic>;
    if (envelope.containsKey('error')) {
      throw PlatformException(
        code: 'ERROR',
        message: envelope['error'] as String?,
      );
    }
    return envelope['ok'] as T?;
  }

  Future<Map<K, V>?> invokeMapMethod<K, V>(
    String method, [
    dynamic arguments,
  ]) async {
    final result = await invokeMethod<Map<dynamic, dynamic>>(method, arguments);
    return result?.cast<K, V>();
  }

  void setMethodCallHandler(
    Future<dynamic> Function(MethodCall call)? handler,
  ) {
    _platform.setMethodCallHandler(handler);
  }
}

typedef _InvokeNative = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>);
typedef _FreeNative = Void Function(Pointer<Utf8>);
typedef _Free = void Function(Pointer<Utf8>);

// Looked up once per isolate; the OS loads the DLL only once.
final _library = DynamicLibrary.open('mornye_mobile.dll');
final _invoke = _library.lookupFunction<_InvokeNative, _InvokeNative>(
  'mornye_desktop_invoke',
);
final _free = _library.lookupFunction<_FreeNative, _Free>(
  'mornye_desktop_free',
);

String _invokeDesktop(String method, String arguments) {
  final nativeMethod = method.toNativeUtf8();
  final nativeArguments = arguments.toNativeUtf8();
  try {
    final reply = _invoke(nativeMethod, nativeArguments);
    try {
      return reply.toDartString();
    } finally {
      _free(reply);
    }
  } finally {
    malloc.free(nativeMethod);
    malloc.free(nativeArguments);
  }
}
