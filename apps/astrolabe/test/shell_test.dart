/// Primary-client chrome: two tiers, one application menu, no duplicate model.
library;

import 'package:astrolabe/src/core/client.dart';
import 'package:astrolabe/src/settings/window.dart';
import 'package:astrolabe/src/shell/caption.dart';
import 'package:astrolabe/src/shell/shell.dart';
import 'package:astrolabe/src/shell/theme.dart';
import 'package:astrolabe/src/surfaces/library.dart';
import 'package:astrolabe/src/shell/window.dart';
import 'package:covalence/covalence.dart';
import 'package:flutter/material.dart' show MaterialApp, Scaffold, ThemeMode;
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

const _view = ClientView(
  loading: false,
  library_: [],
  heads: [],
  devices: [],
  storage: [],
  orbits: [],
  notices: [],
  failures: [],
  inFlight: [],
);

class _WindowControlHost implements WindowControlHost {
  @override
  Future<void> configureOwned(OwnedWindowConfiguration configuration) async {}

  @override
  Future<void> close() async {}

  @override
  Future<void> hide() async {}

  @override
  Future<bool> isMaximized() async => false;

  @override
  Future<void> minimize() async {}

  @override
  Future<void> startDragging() async {}

  @override
  Future<void> toggleMaximize() async {}
}

Future<List<ActionRequest>> _pump(
  WidgetTester tester, {
  required VoidCallback onToggleTheme,
}) async {
  tester.view.physicalSize = const Size(1040, 720);
  tester.view.devicePixelRatio = 1;
  await tester.binding.setSurfaceSize(const Size(1040, 720));
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  addTearDown(() => tester.binding.setSurfaceSize(null));

  final asked = <ActionRequest>[];
  await tester.pumpWidget(
    MaterialApp(
      theme: astrolabeTheme(Brightness.dark),
      home: ClientScope(
        client: Client.canned(_view, onDispatch: asked.add),
        child: WorldSettingsScope(
          onOpen: (_) async {},
          child: Scaffold(
            body: AstrolabeShell(
              themeMode: ThemeMode.dark,
              onToggleTheme: onToggleTheme,
              chrome: _WindowControlHost(),
            ),
          ),
        ),
      ),
    ),
  );
  await tester.pump(const Duration(milliseconds: 300));
  return asked;
}

void main() {
  testWidgets('the client is the Library alone — one tier, no navigation',
      (tester) async {
    await _pump(tester, onToggleTheme: () {});

    expect(
        tester.getSize(find.byType(CaptionControls)).height, kUtilityBarHeight);
    // The destinations that no longer exist draw nothing: the client
    // surfaces lifecycle, the address book is its own window, and each
    // World carries its own settings.
    expect(find.text('Spaces'), findsNothing);
    expect(find.text('Members'), findsNothing);
    expect(find.text('Operations'), findsNothing);
    expect(find.byType(LibrarySurface), findsOneWidget);
    // The address book stays one press away on the utility tier.
    expect(find.bySemanticsLabel('Address book'), findsOneWidget);
    expect(find.text('Refresh local state'), findsNothing);
    expect(find.text('Use light theme'), findsNothing);
    expect(
      find.descendant(
        of: find.widgetWithText(Button, 'ASTROLABE'),
        matching: find.text('Local identity unavailable'),
      ),
      findsNothing,
    );
    expect(find.byIcon(AppIcons.arrowDropDown), findsNothing);
  });

  testWidgets('the Astrolabe wordmark owns refresh and theme settings',
      (tester) async {
    var themeToggles = 0;
    final asked = await _pump(
      tester,
      onToggleTheme: () => themeToggles += 1,
    );

    expect(find.bySemanticsLabel('Astrolabe settings'), findsOneWidget);
    await tester.tap(find.bySemanticsLabel('Astrolabe settings'));
    await tester.pumpAndSettle();
    expect(find.text('Refresh local state'), findsOneWidget);
    expect(find.text('F5'), findsOneWidget);
    expect(find.text('Use light theme'), findsOneWidget);
    expect(find.text('CLIENT SETTINGS'), findsOneWidget);
    expect(
      find.descendant(
        of: find.widgetWithText(Button, 'ASTROLABE'),
        matching: find.text('Local identity unavailable'),
      ),
      findsNothing,
    );
    expect(find.byIcon(AppIcons.arrowDropDown), findsNothing);
    expect(
      tester.widget<Button>(find.widgetWithText(Button, 'ASTROLABE')).active,
      isFalse,
    );

    await tester.tap(find.text('Refresh local state'));
    await tester.pumpAndSettle();
    expect(asked, [const ActionRequest.refresh()]);

    await tester.tap(find.bySemanticsLabel('Astrolabe settings'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Use light theme'));
    await tester.pumpAndSettle();
    expect(themeToggles, 1);
  });

}
