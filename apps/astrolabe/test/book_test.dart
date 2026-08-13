/// The book window, canned: press Refresh, read the ActionRequest.
library;

import 'package:astrolabe/src/core/client.dart';
import 'package:astrolabe/src/shell/book.dart';
import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/material.dart' show MaterialApp, Scaffold, ThemeMode;
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:astrolabe/src/shell/theme.dart';

ClientView _view({List<String> inFlight = const []}) => ClientView(
      loading: false,
      library_: const [],
      heads: const [],
      devices: const [],
      storage: const [],
      orbits: const [],
      notices: const [],
      failures: const [],
      inFlight: inFlight,
    );

void main() {
  testWidgets('refresh on the book asks the core, not a second model',
      (tester) async {
    final asked = <ActionRequest>[];
    final client = Client.canned(
      _view(),
      onDispatch: asked.add,
    );
    await tester.pumpWidget(
      MaterialApp(
        theme: astrolabeTheme(Brightness.light),
        home: ClientScope(
          client: client,
          child: const Scaffold(
            body: BookPage(
              themeMode: ThemeMode.dark,
              onToggleTheme: _noop,
            ),
          ),
        ),
      ),
    );
    await tester.pump(const Duration(milliseconds: 300));
    await tester.tap(find.text('Refresh'));
    expect(asked, [const ActionRequest.refresh()]);
  });

  testWidgets('an in-flight refresh disables the control', (tester) async {
    final asked = <ActionRequest>[];
    final client = Client.canned(
      _view(inFlight: [ActionKeys.refresh]),
      onDispatch: asked.add,
    );
    await tester.pumpWidget(
      MaterialApp(
        theme: astrolabeTheme(Brightness.light),
        home: ClientScope(
          client: client,
          child: const Scaffold(
            body: BookPage(
              themeMode: ThemeMode.dark,
              onToggleTheme: _noop,
            ),
          ),
        ),
      ),
    );
    await tester.pump(const Duration(milliseconds: 300));
    await tester.tap(find.bySemanticsLabel('Refresh'));
    expect(asked, isEmpty);
  });
}

void _noop() {}
