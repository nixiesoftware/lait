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
import 'package:flutter/services.dart' show MethodCall, SystemChannels;
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

/// What one pump produced: what the shell asked the core for, and what it
/// handed the platform's own menu bar.
class _Pumped {
  const _Pumped(this.asked, this.menus);

  final List<ActionRequest> asked;

  /// The `Menu.setMenus` payloads, newest last. Empty off macOS, where the
  /// application menu is drawn in the window instead.
  final List<Map<Object?, Object?>> menus;

  /// Every label in the last menu bar sent, at any depth.
  List<String> get labels {
    final found = <String>[];
    void walk(Object? node) {
      if (node is List) {
        for (final child in node) {
          walk(child);
        }
      } else if (node is Map) {
        final label = node['label'];
        if (label is String) found.add(label);
        walk(node['children']);
      }
    }

    if (menus.isNotEmpty) walk(menus.last['0']);
    return found;
  }

  /// The menu item carrying [label], anywhere in the last bar sent.
  Map<Object?, Object?>? item(String label) {
    Map<Object?, Object?>? search(Object? node) {
      if (node is List) {
        for (final child in node) {
          final hit = search(child);
          if (hit != null) return hit;
        }
      } else if (node is Map) {
        if (node['label'] == label) return node;
        return search(node['children']);
      }
      return null;
    }

    return menus.isEmpty ? null : search(menus.last['0']);
  }
}

Future<_Pumped> _pump(
  WidgetTester tester, {
  required VoidCallback onToggleTheme,
  ClientView view = _view,
}) async {
  tester.view.physicalSize = const Size(1040, 720);
  tester.view.devicePixelRatio = 1;
  await tester.binding.setSurfaceSize(const Size(1040, 720));
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  addTearDown(() => tester.binding.setSurfaceSize(null));

  // The platform menu bar is a real channel, and nothing answers it in a
  // test — so this both keeps it quiet and is how the bar is read back.
  final menus = <Map<Object?, Object?>>[];
  tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
    SystemChannels.menu,
    (call) async {
      if (call.method == 'Menu.setMenus') {
        menus.add((call.arguments as Map).cast<Object?, Object?>());
      }
      return null;
    },
  );
  addTearDown(() => tester.binding.defaultBinaryMessenger
      .setMockMethodCallHandler(SystemChannels.menu, null));

  final asked = <ActionRequest>[];
  await tester.pumpWidget(
    MaterialApp(
      theme: astrolabeTheme(Brightness.dark),
      home: ClientScope(
        client: Client.canned(view, onDispatch: asked.add),
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
  return _Pumped(asked, menus);
}

/// Which chrome is right is a fact about the host, so every test below pins
/// one. A test that read the machine it happened to run on would pass on one
/// developer's desk and fail in CI for a reason neither could see.
final _windows = TargetPlatformVariant.only(TargetPlatform.windows);
final _macOS = TargetPlatformVariant.only(TargetPlatform.macOS);

void main() {
  group('where the window draws its own chrome', _drawnChrome);
  group('where the system draws it', _systemChrome);
}

/// Windows: the caption carries the cluster and the wordmark that opens the
/// application menu, because the operating system offers neither.
void _drawnChrome() {
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
  }, variant: _windows);

  testWidgets('the Astrolabe wordmark owns refresh and theme settings',
      (tester) async {
    var themeToggles = 0;
    final pumped = await _pump(
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
    expect(pumped.asked, [const ActionRequest.refresh()]);

    await tester.tap(find.bySemanticsLabel('Astrolabe settings'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Use light theme'));
    await tester.pumpAndSettle();
    expect(themeToggles, 1);

    // Nothing was sent to a platform menu bar: there is none to send to.
    expect(pumped.menus, isEmpty);
  }, variant: _windows);
}

/// macOS: the traffic lights are the window's controls and the screen's own
/// bar is its application menu, so the caption draws neither.
void _systemChrome() {
  testWidgets('the window draws no controls the system already draws',
      (tester) async {
    await _pump(tester, onToggleTheme: () {});

    expect(find.byType(CaptionControls), findsNothing);
    expect(find.bySemanticsLabel('Minimise'), findsNothing);
    expect(find.bySemanticsLabel('Maximise'), findsNothing);
    expect(find.bySemanticsLabel('Close'), findsNothing);
    // The Library is unchanged, and so is the one affordance the utility tier
    // keeps: only the chrome around them moved.
    expect(find.byType(LibrarySurface), findsOneWidget);
    expect(find.bySemanticsLabel('Address book'), findsOneWidget);
  }, variant: _macOS);

  testWidgets('the wordmark is gone and its corner is the traffic lights\'',
      (tester) async {
    await _pump(tester, onToggleTheme: () {});

    expect(find.text('ASTROLABE'), findsNothing);
    expect(find.bySemanticsLabel('Astrolabe settings'), findsNothing);

    // Nothing of ours starts before the cluster the system draws there.
    final book = tester.getRect(find.bySemanticsLabel('Address book'));
    expect(book.left, greaterThanOrEqualTo(kTrafficLightSpan));
  }, variant: _macOS);

  testWidgets('the settings the wordmark held are on the screen\'s own bar',
      (tester) async {
    var themeToggles = 0;
    final pumped = await _pump(
      tester,
      onToggleTheme: () => themeToggles += 1,
    );

    expect(pumped.menus, isNotEmpty);
    expect(
      pumped.labels,
      containsAll(<String>[
        'Astrolabe',
        'Refresh local state',
        'Use light theme',
        'Address book',
      ]),
    );

    // The item is live, and it reaches the same core the drawn one did.
    final refresh = pumped.item('Refresh local state')!;
    expect(refresh['enabled'], isTrue);
    tester.binding.defaultBinaryMessenger.handlePlatformMessage(
      SystemChannels.menu.name,
      SystemChannels.menu.codec.encodeMethodCall(
        MethodCall('Menu.selectedCallback', refresh['id']),
      ),
      (_) {},
    );
    await tester.pump();
    expect(pumped.asked, [const ActionRequest.refresh()]);

    final theme = pumped.item('Use light theme')!;
    tester.binding.defaultBinaryMessenger.handlePlatformMessage(
      SystemChannels.menu.name,
      SystemChannels.menu.codec.encodeMethodCall(
        MethodCall('Menu.selectedCallback', theme['id']),
      ),
      (_) {},
    );
    await tester.pump();
    expect(themeToggles, 1);
  }, variant: _macOS);

  testWidgets('a re-read already in flight is an item that cannot be chosen',
      (tester) async {
    final pumped = await _pump(
      tester,
      onToggleTheme: () {},
      view: const ClientView(
        loading: false,
        library_: [],
        heads: [],
        devices: [],
        storage: [],
        orbits: [],
        notices: [],
        failures: [],
        inFlight: [ActionKeys.refresh],
      ),
    );

    expect(pumped.item('Refresh local state')!['enabled'], isFalse);
  }, variant: _macOS);
}
