import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mornye/services/backend_channel.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  const name = 'com.zarz.mornye/backend';
  const channel = BackendChannel(name);
  final messenger =
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;

  tearDown(() {
    messenger.setMockMethodCallHandler(const MethodChannel(name), null);
  });

  // Desktop hosts call the Rust library directly in the app; under tests the
  // channel must stay mockable on every host, Windows included.
  test('routes calls through the mockable method channel', () async {
    final calls = <MethodCall>[];
    messenger.setMockMethodCallHandler(const MethodChannel(name), (call) async {
      calls.add(call);
      return call.method == 'getBackendImplementations'
          ? {'filename': 'rust'}
          : 'reply';
    });

    expect(
      await channel.invokeMethod<String>('sanitizeFilename', {'filename': 'a'}),
      'reply',
    );
    expect(
      await channel.invokeMapMethod<String, String>(
        'getBackendImplementations',
      ),
      {'filename': 'rust'},
    );
    expect(calls.map((call) => call.method), [
      'sanitizeFilename',
      'getBackendImplementations',
    ]);
    expect(calls.first.arguments, {'filename': 'a'});
  });

  test('keeps platform errors as PlatformException', () async {
    messenger.setMockMethodCallHandler(const MethodChannel(name), (call) async {
      throw PlatformException(code: 'ERROR', message: 'not initialized');
    });

    await expectLater(
      channel.invokeMethod<void>('getInstalledExtensions'),
      throwsA(
        isA<PlatformException>()
            .having((error) => error.code, 'code', 'ERROR')
            .having((error) => error.message, 'message', 'not initialized'),
      ),
    );
  });
}
