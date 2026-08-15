/// The window host: typed owned windows, never one engine per summons.
library;

import 'package:astrolabe/src/shell/host.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('only the book argument names the book window', () {
    expect(isBookWindow(bookWindowArgument), isTrue);
    expect(isBookWindow(''), isFalse);
    expect(isBookWindow('--world-settings=x'), isFalse);
  });

  test('owned routes carry a stable native key and exact engine arguments', () {
    const book = OwnedWindowRoute.addressBook();
    const settings = OwnedWindowRoute(
      key: 'world-settings:orb_one/issues',
      arguments: '--world-settings=encoded',
    );

    expect(book.key, bookWindowKey);
    expect(book.matches(bookWindowArgument), isTrue);
    expect(settings.matches('--world-settings=encoded'), isTrue);
    expect(settings.matches('--world-settings=stale'), isFalse);
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
