/// The window host: one book window, never one per summons.
library;

import 'package:astrolabe/src/shell/host.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('only the book argument names the book window', () {
    expect(isBookWindow(bookWindowArgument), isTrue);
    expect(isBookWindow(''), isFalse);
    expect(isBookWindow('--world-settings=x'), isFalse);
  });

  test('a sub-engine is recognised from the plugin argv, not the channel', () {
    expect(
      isBookEngine(['multi_window', 'abc', bookWindowArgument]),
      isTrue,
    );
    expect(isBookEngine([]), isFalse);
    expect(isSubEngine(['multi_window', 'abc', 'other']), isTrue);
    expect(isSubEngine([]), isFalse);
  });
}
