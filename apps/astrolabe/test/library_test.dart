/// Widget tests over the same surfaces the shell draws.
///
/// These run with no display, no bridge, no core, no daemon and no window —
/// which is the property the retiring interface's interaction tests had, and
/// the one worth keeping through the move. A test builds a `ClientView` (an
/// ordinary Dart object), pumps a surface with it, presses a real control, and
/// reads what the surface asked for.
///
/// The states covered are the ones that are easy to skip and expensive to get
/// wrong: loading against empty, the two reasons a row cannot be opened, and
/// the one that has to work — `Open` naming the row the pane is about.
library;

import 'package:astrolabe/src/core/client.dart';
import 'package:astrolabe/src/settings/window.dart';
import 'package:astrolabe/src/surfaces/library.dart';
import 'package:astrolabe/src/surfaces/surfaces.dart' as astrolabe;
import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/material.dart' show MaterialApp, Scaffold;
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

ClientView _view({
  List<LibraryRow>? library,
  List<HeadRow> heads = const [],
  List<String> inFlight = const [],
}) =>
    ClientView(
      loading: library == null,
      stale: library == null ? const Staleness.neverLoaded() : null,
      library_: library,
      heads: heads,
      devices: const [],
      storage: const [],
      orbits: const [],
      notices: const [],
      failures: const [],
      inFlight: inFlight,
    );

LibraryRow _row({
  required String orbit,
  String mount = 'issues',
  String? name,
  PlacementView placement = PlacementView.vacant,
  String? opensAt = '/',
  Unopenable? unopenable,
  String? tagline,
  BigInt? lastOpened,
  String? store,
  int? version,
  String? syncState,
  String? syncDetail,
  List<RouteRow> routes = const [],
}) =>
    LibraryRow(
      key: '$orbit/$mount',
      orbit: orbit,
      space: orbit,
      worldMount: mount,
      displayName: name,
      placement: placement,
      opensAt: opensAt,
      unopenable: unopenable,
      lastOpened: lastOpened,
      store: store,
      version: version,
      syncState: syncState,
      syncDetail: syncDetail,
      tagline: tagline,
      routes: routes,
    );

Future<List<ActionRequest>> _pump(
  WidgetTester tester,
  ClientView view, {
  OpenWorldSettings? onSettings,
}) async {
  final asked = <ActionRequest>[];
  await tester.pumpWidget(
    MaterialApp(
      theme: covalenceTheme(const ThemeConfig()),
      home: ClientScope(
        client: Client.canned(view, onDispatch: asked.add),
        child: WorldSettingsScope(
          onOpen: onSettings ?? (_) async {},
          child: const Scaffold(
              body: Padding(
            padding: EdgeInsets.all(16),
            child: LibrarySurface(),
          )),
        ),
      ),
    ),
  );
  // Pumped rather than settled. A loading skeleton shimmers and a busy button
  // spins, and both are indefinite by design — `pumpAndSettle` waits for an
  // animation that is never going to end, which reads as the surface hanging
  // when it is the surface working.
  await tester.pump(const Duration(milliseconds: 300));
  return asked;
}

Future<void> _pumpLibraryPage(WidgetTester tester, ClientView view) async {
  tester.view.physicalSize = const Size(1040, 720);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);

  await tester.pumpWidget(
    MaterialApp(
      theme: covalenceTheme(const ThemeConfig()),
      home: ClientScope(
        client: Client.canned(view),
        child: const Scaffold(
          body: astrolabe.SurfacePage(
            surface: astrolabe.Surface.library,
          ),
        ),
      ),
    ),
  );
  await tester.pump(const Duration(milliseconds: 300));
}

void main() {
  testWidgets('Library rail and hero own the frame edges', (tester) async {
    await _pumpLibraryPage(
      tester,
      _view(library: [_row(orbit: 'orb_one', name: 'IssueWorld')]),
    );

    final rail = find.byKey(const ValueKey('library-rail'));
    final hero = find.byKey(const ValueKey('library-hero'));
    final openBand = find.byKey(const ValueKey('library-open-band'));
    final content = find.byKey(const ValueKey('library-detail-content'));

    expect(tester.getTopLeft(rail), Offset.zero);
    expect(tester.getSize(rail).width, kRailWidth);
    expect(tester.getTopLeft(hero), const Offset(kRailWidth, 0));
    expect(tester.getSize(hero).width, 1040 - kRailWidth);
    expect(tester.getTopLeft(openBand), const Offset(kRailWidth, kHeroHeight));
    expect(tester.getSize(openBand).width, 1040 - kRailWidth);
    expect(tester.getTopLeft(content).dx, kRailWidth + 20);
    expect(tester.getTopLeft(content).dy, greaterThan(kHeroHeight + 20));
  });

  testWidgets('loading and empty are told apart on screen', (tester) async {
    await _pump(tester, _view());
    expect(
      find.text('This device serves no Worlds yet.'),
      findsNothing,
      reason: 'a client that has read nothing yet claimed the read was empty',
    );

    await _pump(tester, _view(library: []));
    expect(
      find.text('This device serves no Worlds yet.'),
      findsOneWidget,
      reason: 'an answered-and-empty read said nothing at all',
    );
  });

  testWidgets('a Space that is not running is still openable', (tester) async {
    // The defect the page shipped with: a vacant Orbit activates nothing, so
    // every row is a Space row — and treating those as Worlds that failed to
    // declare an entry path made every row unopenable on a fresh daemon.
    final asked = await _pump(
      tester,
      _view(library: [_row(orbit: 'orb_one', mount: '', name: 'Work')]),
    );

    await tester.tap(find.text('Launch'));
    await tester.pump();

    expect(asked, hasLength(1));
    expect(
      asked.single,
      const ActionRequest.open(orbit: 'orb_one', entryPath: '/'),
      reason: 'opening a Space asked for somewhere other than its front door',
    );
  });

  testWidgets('a World that declares no entry path cannot be opened',
      (tester) async {
    final asked = await _pump(
      tester,
      _view(library: [
        _row(
          orbit: 'orb_one',
          name: 'Issues',
          opensAt: null,
          unopenable: Unopenable.undeclared,
        ),
      ]),
    );

    expect(find.text('Launch'), findsNothing);
    expect(
      asked,
      isEmpty,
      reason: 'a World with no declared entry path offered a live Open control',
    );
  });

  testWidgets('Open follows the selection rather than the page',
      (tester) async {
    // The assertion master–detail has to earn: one primary control instead of
    // one per row means the control has to follow the selection, and a page
    // where it silently did not would open the wrong World while looking
    // entirely correct.
    final asked = await _pump(
      tester,
      _view(library: [
        _row(orbit: 'orb_one', name: 'First'),
        _row(orbit: 'orb_two', name: 'Second', opensAt: '/spaces/two'),
      ]),
    );

    await tester.tap(find.text('Second'));
    await tester.pump(const Duration(milliseconds: 300));
    expect(
      asked,
      isEmpty,
      reason: 'choosing a row in the rail did something — listing is passive',
    );

    await tester.tap(find.text('Launch'));
    await tester.pump();
    expect(
      asked.single,
      const ActionRequest.open(orbit: 'orb_two', entryPath: '/spaces/two'),
      reason: 'Open followed the page rather than the selection',
    );
  });

  testWidgets('a declared route opens at the path the World named',
      (tester) async {
    // The template is the World's, and the client only draws it. A route that
    // resolved to a path this side invented would be the client holding a copy
    // of a URL grammar it does not own.
    final asked = await _pump(
      tester,
      _view(library: [
        _row(
          orbit: 'orb_one',
          name: 'Issues',
          routes: const [
            RouteRow(label: 'Board', path: '/spaces/orb_one/board')
          ],
        ),
      ]),
    );

    await tester.tap(find.text('Board'));
    await tester.pump();
    expect(
      asked.single,
      const ActionRequest.open(
        orbit: 'orb_one',
        entryPath: '/spaces/orb_one/board',
      ),
    );
  });

  testWidgets('a row with no template draws none of it', (tester) async {
    // A Space row, and a World this build does not host, both arrive with an
    // empty template. Nothing is invented to fill the space.
    await _pump(
      tester,
      _view(library: [_row(orbit: 'orb_one', mount: '', name: 'Work')]),
    );
    expect(find.text('GO STRAIGHT TO'), findsNothing);
  });

  testWidgets('a head address is drawn without its credential', (tester) async {
    await _pump(
      tester,
      _view(
        library: [_row(orbit: 'orb_one', name: 'Issues')],
        heads: const [
          HeadRow(
            id: 'identity:default',
            kind: 'browser',
            origin: 'http://127.0.0.1:52713/',
            owned: true,
          ),
        ],
      ),
    );

    expect(find.text('http://127.0.0.1:52713/'), findsOneWidget);
    expect(
      find.textContaining('token='),
      findsNothing,
      reason: 'a run credential reached the front page',
    );
  });

  testWidgets('a control is disabled while its own action is in flight',
      (tester) async {
    final asked = await _pump(
      tester,
      _view(
        library: [_row(orbit: 'orb_one', name: 'Issues')],
        inFlight: const ['open:orb_one/'],
      ),
    );

    expect(find.text('Launching'), findsWidgets);
    expect(find.text('Launch'), findsNothing);
    expect(asked, isEmpty);
  });

  testWidgets('a running World offers icon-only Go to at its declared entry',
      (tester) async {
    final asked = await _pump(
      tester,
      _view(
        library: [
          _row(
            orbit: 'orb_one',
            name: 'Issues',
            placement: PlacementView.placed,
            opensAt: '/spaces/orb_one',
          ),
        ],
      ),
    );

    expect(find.text('Go to'), findsNothing);
    expect(find.text('Running'), findsWidgets);
    expect(find.text('Cancel'), findsNothing);
    expect(find.text('Stop'), findsNothing);

    final goTo = find.widgetWithIcon(Button, AppIcons.openInNew);
    expect(goTo, findsOneWidget);
    final button = tester.widget<Button>(goTo);
    expect(button.label, isNull);
    expect(button.variant, ButtonVariant.ghost);
    expect(button.size, ButtonSize.icon);
    expect(button.semanticLabel, 'Go to running World');

    await tester.tap(goTo);
    await tester.pump();
    expect(
      asked.single,
      const ActionRequest.open(
        orbit: 'orb_one',
        entryPath: '/spaces/orb_one',
      ),
      reason: 'Go to bypassed the World-declared entry path',
    );
  });

  testWidgets('an in-flight vacant World is visibly starting', (tester) async {
    await _pump(
      tester,
      _view(
        library: [_row(orbit: 'orb_one', name: 'Issues')],
        inFlight: const ['open:orb_one/'],
      ),
    );

    expect(find.text('Launching'), findsWidgets);
    expect(find.text('Cancel'), findsNothing);
  });

  testWidgets('the stable row reports runtime facts and settings',
      (tester) async {
    final opened = DateTime.now().toUtc().subtract(const Duration(minutes: 2));
    final settings = <WorldSettingsSnapshot>[];
    await _pump(
      tester,
      _view(
        library: [
          _row(
            orbit: 'orb_one',
            name: 'Issues',
            placement: PlacementView.placed,
            lastOpened: BigInt.from(opened.millisecondsSinceEpoch ~/ 1000),
            store: r'D:\Worlds\issues',
            version: 7,
            syncState: 'pass',
            syncDetail: '4 scopes, 28 items',
          ),
        ],
        heads: const [
          HeadRow(
            id: 'identity:default',
            kind: 'browser',
            origin: 'http://127.0.0.1:52713/',
            owned: true,
          ),
        ],
      ),
      onSettings: (snapshot) async => settings.add(snapshot),
    );

    expect(find.text('Up to date'), findsOneWidget);
    expect(find.text('v7'), findsOneWidget);
    expect(find.text('2 minutes ago'), findsOneWidget);

    await tester.tap(find.byIcon(AppIcons.settings));
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('Issues settings'), findsNothing);
    expect(settings, hasLength(1));
    expect(settings.single.name, 'Issues');
    expect(settings.single.store, r'D:\Worlds\issues');
    expect(settings.single.syncDetail, '4 scopes, 28 items');
    expect(settings.single.activeOrigin, 'http://127.0.0.1:52713/');
  });

  testWidgets('World settings render as a standalone surface', (tester) async {
    const snapshot = WorldSettingsSnapshot(
      key: 'orb_one/issues',
      name: 'Issues',
      orbit: 'orb_one',
      syncLabel: 'Up to date',
      syncDetail: '4 scopes, 28 items',
      store: r'D:\Worlds\issues',
      worldMount: 'issues',
      entryPath: '/spaces/orb_one',
      version: 7,
      activeOrigin: 'http://127.0.0.1:52713/',
      dark: true,
    );
    await tester.pumpWidget(
      MaterialApp(
        theme: covalenceTheme(const ThemeConfig()),
        home: const Scaffold(body: WorldSettingsPage(snapshot: snapshot)),
      ),
    );
    await tester.pump();

    expect(find.text('Issues settings'), findsOneWidget);
    expect(find.text(r'D:\Worlds\issues'), findsOneWidget);
    expect(find.text('4 scopes, 28 items'), findsOneWidget);
    expect(find.text('http://127.0.0.1:52713/'), findsOneWidget);
  });

  testWidgets('World settings own an Astrolabe window shell', (tester) async {
    const snapshot = WorldSettingsSnapshot(
      key: 'orb_one/issues',
      name: 'Issues',
      orbit: 'orb_one',
      syncLabel: 'Up to date',
      syncDetail: '4 scopes, 28 items',
      store: r'D:\Worlds\issues',
      worldMount: 'issues',
      entryPath: '/spaces/orb_one',
      version: 7,
      activeOrigin: 'http://127.0.0.1:52713/',
      dark: true,
    );
    await tester.pumpWidget(const WorldSettingsApp(snapshot: snapshot));
    await tester.pump();

    expect(
      find.byKey(const ValueKey('world-settings-window-shell')),
      findsOneWidget,
    );
    expect(find.text('ASTROLABE'), findsOneWidget);
    expect(find.text('Issues settings'), findsNWidgets(2));
    expect(find.bySemanticsLabel('Close'), findsOneWidget);
  });

  test('World settings survive the child-process argument crossing', () {
    const snapshot = WorldSettingsSnapshot(
      key: 'orb_one/issues',
      name: 'Issues',
      orbit: 'orb_one',
      syncLabel: 'Up to date',
      syncDetail: '4 scopes, 28 items',
      store: r'D:\Worlds\issues',
      worldMount: 'issues',
      entryPath: '/spaces/orb_one',
      version: 7,
      activeOrigin: 'http://127.0.0.1:52713/',
      dark: true,
    );

    final decoded = WorldSettingsSnapshot.fromArguments([
      '--unrelated',
      snapshot.toArgument(),
    ]);
    expect(decoded?.key, snapshot.key);
    expect(decoded?.store, snapshot.store);
    expect(decoded?.activeOrigin, snapshot.activeOrigin);
    expect(decoded?.dark, isTrue);
  });

  testWidgets('Library search filters the passive rail', (tester) async {
    final asked = await _pump(
      tester,
      _view(
        library: [
          _row(orbit: 'orb_one', name: 'IssueWorld'),
          _row(orbit: 'orb_two', name: 'Notes'),
        ],
      ),
    );

    await tester.enterText(find.byType(Input), 'notes');
    await tester.pump();

    expect(find.text('IssueWorld'), findsNothing);
    expect(find.text('Notes'), findsWidgets);
    expect(
      asked,
      isEmpty,
      reason: 'filtering the Library placed or opened a World',
    );
  });
}
