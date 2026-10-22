// Source contract test.
//
// SQLite reads "" as an identifier and only falls back to a string literal
// when the legacy DQS option is enabled. Android's system SQLite enables it,
// but SQLite builds without it (such as desktop sqlite3 through
// sqflite_common_ffi) fail with "no such column". SQL in lib/ must spell an
// empty string as ''.

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';

void main() {
  test('SQL compares with empty strings using single quotes', () {
    final emptyDoubleQuoted = RegExp(r'''(=|!=|<>)\s*""''');
    final offenders = <String>[];
    for (final file in Directory('lib').listSync(recursive: true)) {
      if (file is! File || !file.path.endsWith('.dart')) continue;
      final lines = file.readAsLinesSync();
      for (var index = 0; index < lines.length; index++) {
        if (emptyDoubleQuoted.hasMatch(lines[index])) {
          offenders.add('${file.path}:${index + 1}: ${lines[index].trim()}');
        }
      }
    }
    expect(offenders, isEmpty);
  });
}
