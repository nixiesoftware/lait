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
import 'package:astrolabe/src/shell/person.dart';
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
  List<WorldPersonRow>? people,
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
      people: people,
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

    await tester.tap(find.text('LAUNCH'));
    await tester.pump();

    expect(tester.widget<Text>(find.text('LAUNCH')).style?.fontSize, 20);
    expect(
      tester.widget<Icon>(find.byIcon(AppIcons.playArrow)).size,
      20,
    );
    final launchButton = tester.widget<Button>(
      find.ancestor(
        of: find.text('LAUNCH'),
        matching: find.byType(Button),
      ),
    );
    expect(launchButton.borderRadius, BorderRadius.circular(2));
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

    expect(find.text('LAUNCH'), findsNothing);
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

    await tester.tap(find.text('LAUNCH'));
    await tester.pump();
    expect(
      asked.single,
      const ActionRequest.open(orbit: 'orb_two', entryPath: '/spaces/two'),
      reason: 'Open followed the page rather than the selection',
    );
  });

  testWidgets('a declared route draws no navigation — lifecycle only',
      (tester) async {
    // The template still declares its routes, and the client still refuses
    // to surface them: Board/Issues/Specs are the World's own navigation,
    // and drawing them here would cross the one boundary this client keeps.
    // Open is the whole act.
    await _pump(
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

    expect(find.text('Board'), findsNothing);
  });

  testWidgets('the glance is the book joined to this World', (tester) async {
    // The reference client's "friends who play", resolved through the book,
    // in two tiers of liveness: a canonical row for whoever has the World
    // open right now, and a bare face for whoever merely holds it — the
    // name travelling as a tooltip, never a bespoke row.
    await _pump(
      tester,
      _view(library: [
        _row(
          orbit: 'orb_one',
          name: 'Issues',
          // A reported sync gate, so the action band's own wording cannot
          // collide with the presence labels this test is about.
          syncState: 'pass',
          people: const [
            WorldPersonRow(
              name: 'Moon',
              presence: PresenceView.offline,
              agent: false,
              here: false,
            ),
            WorldPersonRow(
              name: 'claude',
              presence: PresenceView.online,
              agent: true,
              here: true,
            ),
          ],
        ),
      ]),
    );

    // No panel head and no count: the two tier lines beneath already say
    // who is in the World and who merely holds it.
    expect(find.textContaining('IN THIS WORLD'), findsNothing);
    expect(find.text('1 is in the World now'), findsOneWidget);
    expect(find.text('1 has it in their library'), findsOneWidget);
    // The launched tier draws the one canonical tile; the holding tier
    // draws only the face, its name in the tooltip.
    expect(find.byType(PersonTile), findsOneWidget);
    expect(find.text('claude'), findsOneWidget);
    expect(find.byType(AiMark), findsOneWidget);
    expect(find.text('Moon'), findsNothing);
    expect(
      find.bySemanticsLabel('Moon — Offline'),
      findsOneWidget,
      reason: 'the bare face still names and measures its person',
    );
    expect(find.text('Online'), findsOneWidget);
  });

  testWidgets('the glance keeps its two absences apart', (tester) async {
    // An unread book and a Space nobody in the book is addressed in are
    // different facts, and the panel says which it is.
    await _pump(
      tester,
      _view(library: [_row(orbit: 'orb_one', name: 'Issues')]),
    );
    expect(find.text('The book has not been read.'), findsOneWidget);

    await _pump(
      tester,
      _view(library: [
        _row(orbit: 'orb_one', name: 'Issues', people: const []),
      ]),
    );
    expect(find.text('Nobody in the book is addressed here.'), findsOneWidget);
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

    expect(find.text('LAUNCHING'), findsOneWidget);
    expect(find.text('LAUNCH'), findsNothing);
    expect(
      tester.widget<Text>(find.text('LAUNCHING')).style?.fontSize,
      20,
    );
    expect(
      tester.widget<Text>(find.text('LAUNCHING')).style?.fontWeight,
      FontWeight.w400,
    );
    expect(
        tester.widget<Progress>(find.byType(Progress)).size, ProgressSize.lg);
    expect(asked, isEmpty);
  });

  testWidgets('a running World is one solid split control, both halves Go to',
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
    expect(find.text('RUNNING'), findsWidgets);
    expect(find.text('Cancel'), findsNothing);
    expect(find.text('Stop'), findsNothing);

    final openBand = find.byKey(const ValueKey('library-open-band'));
    final runningLabel = find.descendant(
      of: openBand,
      matching: find.text('RUNNING'),
    );
    expect(tester.widget<Text>(runningLabel).style?.fontSize, 20);
    expect(
        tester.widget<Text>(runningLabel).style?.fontWeight, FontWeight.w400);

    // The reference client's anatomy: one solid slab — a play mark and the
    // state, a hairline, the handoff glyph — not a status chip beside a
    // detached ghost button.
    final playMark = find.descendant(
      of: openBand,
      matching: find.byIcon(AppIcons.playArrow),
    );
    expect(playMark, findsOneWidget);
    expect(tester.widget<Icon>(playMark).size, 20);
    // White ink on the vivid fill, in either theme.
    expect(
      tester.widget<Text>(runningLabel).style?.color,
      const Color(0xFFFFFFFF),
    );
    expect(
      find.descendant(
        of: openBand,
        matching: find.byIcon(AppIcons.checkCircle),
      ),
      findsNothing,
    );
    expect(find.widgetWithIcon(Button, AppIcons.openInNew), findsNothing);
    final handoff = find.descendant(
      of: openBand,
      matching: find.byIcon(AppIcons.openInNew),
    );
    expect(handoff, findsOneWidget);
    expect(
      tester.widget<Icon>(handoff).size,
      tester.widget<Icon>(playMark).size,
      reason: 'the slab\'s two glyphs share one size',
    );
    expect(tester.getCenter(handoff).dy, tester.getCenter(runningLabel).dy);

    // Both segments are the same act, at the World-declared entry path.
    await tester.tap(runningLabel);
    await tester.pump();
    await tester.tap(handoff);
    await tester.pump();
    expect(
      asked,
      const [
        ActionRequest.open(orbit: 'orb_one', entryPath: '/spaces/orb_one'),
        ActionRequest.open(orbit: 'orb_one', entryPath: '/spaces/orb_one'),
      ],
      reason: 'a segment bypassed the World-declared entry path',
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

    expect(find.text('LAUNCHING'), findsOneWidget);
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
    expect(find.text('ASTROLABE'), findsNothing);
    expect(find.text('Issues settings'), findsNWidgets(2));
    final chrome = tester.widget<WindowChrome>(find.byType(WindowChrome));
    expect(chrome.role, WindowChromeRole.secondary);
    expect(chrome.identity, isNull);
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
