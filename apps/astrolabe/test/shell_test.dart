/// Primary-client chrome: two tiers, one application menu, no duplicate model.
library;

import 'package:astrolabe/src/core/client.dart';
import 'package:astrolabe/src/settings/window.dart';
import 'package:astrolabe/src/shell/caption.dart';
import 'package:astrolabe/src/shell/shell.dart';
import 'package:astrolabe/src/shell/theme.dart';
import 'package:astrolabe/src/shell/window.dart';
import 'package:astrolabe/src/surfaces/surfaces.dart' show Surface;
import 'package:covalence/covalence.dart' hide Surface, WindowChrome;
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

class _WindowChrome implements WindowChrome {
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
              chrome: _WindowChrome(),
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
  testWidgets('primary header has distinct utility and navigation tiers',
      (tester) async {
    await _pump(tester, onToggleTheme: () {});

    expect(
        tester.getSize(find.byType(CaptionControls)).height, kUtilityBarHeight);
    expect(
        tester.getSize(find.byType(Tabs<Surface>)).height, kPrimaryBarHeight);
    expect(find.text('Refresh local state'), findsNothing);
    expect(find.text('Use light theme'), findsNothing);
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

    await tester.tap(find.text('Refresh local state'));
    await tester.pumpAndSettle();
    expect(asked, [const ActionRequest.refresh()]);

    await tester.tap(find.bySemanticsLabel('Astrolabe settings'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Use light theme'));
    await tester.pumpAndSettle();
    expect(themeToggles, 1);
  });

  testWidgets('the lower tier remains the primary surface navigation',
      (tester) async {
    await _pump(tester, onToggleTheme: () {});

    await tester.tap(find.text('Operations'));
    await tester.pumpAndSettle();

    expect(find.text('OPERATIONS'), findsOneWidget);
    expect(find.text('Devices'), findsNWidgets(2));
    expect(find.text('Heads'), findsOneWidget);
    expect(find.text('Storage'), findsOneWidget);
    expect(find.text('Diagnostics'), findsOneWidget);
  });
}
