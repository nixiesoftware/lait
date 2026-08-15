/// Widget tests over the same surfaces the shell draws.
///
/// These run with no display, no bridge, no core, no daemon and no window —
/// which is the property the retiring interface's interaction tests had, and
/// the one worth keeping through the move. A test builds a `ClientView` (an
/// ordinary Dart object), pumps a surface with it, presses a real control, and
/// reads what the surface asked for.
///
/// The states covered are the ones that are easy to skip and expensive to get
/// wrong: loading against empty, the row that cannot be opened, and the one
/// that has to work — `Open` naming the row the pane is about.
library;

import 'package:astrolabe/src/core/client.dart';
import 'package:astrolabe/src/settings/window.dart';
import 'package:astrolabe/src/shell/person.dart';
import 'package:astrolabe/src/surfaces/library.dart';
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
  String mount = 'issues',
  String name = 'Issues',
  String? opensAt = '/',
  String? tagline,
  int? version,
  List<WorldPersonRow>? people,
}) =>
    LibraryRow(
      key: mount,
      worldMount: mount,
      displayName: name,
      opensAt: opensAt,
      version: version,
      tagline: tagline,
      people: people,
    );

/// The one head an identity serves its Worlds through. `orbit` stays null —
/// that is what marks it as the identity-wide browser head the Library reads
/// "running" from.
const HeadRow _identityHead = HeadRow(
  id: 'identity:default',
  kind: 'browser',
  origin: 'http://127.0.0.1:52713/',
  owned: true,
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
          body: LibrarySurface(),
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
      _view(library: [_row(name: 'IssueWorld')]),
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
      find.text('This build installs no Worlds.'),
      findsNothing,
      reason: 'a client that has read nothing yet claimed the read was empty',
    );

    await _pump(tester, _view(library: []));
    expect(
      find.text('This build installs no Worlds.'),
      findsOneWidget,
      reason: 'an answered-and-empty read said nothing at all',
    );
  });

  testWidgets('Launch asks for the World-declared entry path', (tester) async {
    final asked = await _pump(
      tester,
      _view(library: [_row()]),
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
      const ActionRequest.open(entryPath: '/'),
      reason: 'opening asked for somewhere other than the declared entry',
    );
  });

  testWidgets('a World that declares no entry path cannot be opened',
      (tester) async {
    final asked = await _pump(
      tester,
      _view(library: [_row(opensAt: null)]),
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
        _row(mount: 'issues', name: 'First'),
        _row(mount: 'notes', name: 'Second', opensAt: '/notes'),
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
      const ActionRequest.open(entryPath: '/notes'),
      reason: 'Open followed the page rather than the selection',
    );
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
    // An unread book and a World nobody in the book is addressed near are
    // different facts, and the panel says which it is.
    await _pump(
      tester,
      _view(library: [_row()]),
    );
    expect(find.text('The book has not been read.'), findsOneWidget);

    await _pump(
      tester,
      _view(library: [_row(people: const [])]),
    );
    expect(find.text('Nobody in the book is addressed here.'), findsOneWidget);
  });

  testWidgets('a head address is drawn without its credential', (tester) async {
    await _pump(
      tester,
      _view(
        library: [_row()],
        heads: const [_identityHead],
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
        library: [_row()],
        inFlight: const ['open:/'],
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

  testWidgets('a serving head is one solid split control, both halves Go to',
      (tester) async {
    // "Running" is the identity head's own liveness: the destination is up,
    // and Open is a handoff rather than a start. There is no per-Orbit badge
    // to read it from any more, deliberately — Space lifecycle belongs to the
    // head's own front page.
    final asked = await _pump(
      tester,
      _view(
        library: [_row(opensAt: '/issues')],
        heads: const [_identityHead],
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
        ActionRequest.open(entryPath: '/issues'),
        ActionRequest.open(entryPath: '/issues'),
      ],
      reason: 'a segment bypassed the World-declared entry path',
    );
  });

  testWidgets('an in-flight open is visibly launching', (tester) async {
    await _pump(
      tester,
      _view(
        library: [_row()],
        inFlight: const ['open:/'],
      ),
    );

    expect(find.text('LAUNCHING'), findsOneWidget);
    expect(find.text('Cancel'), findsNothing);
  });

  testWidgets('the row reports its declaration and opens settings',
      (tester) async {
    final settings = <WorldSettingsSnapshot>[];
    await _pump(
      tester,
      _view(
        library: [_row(version: 7)],
        heads: const [_identityHead],
      ),
      onSettings: (snapshot) async => settings.add(snapshot),
    );

    expect(find.text('v7'), findsOneWidget);

    await tester.tap(find.byIcon(AppIcons.settings));
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('Issues settings'), findsNothing);
    expect(settings, hasLength(1));
    expect(settings.single.name, 'Issues');
    expect(settings.single.worldMount, 'issues');
    expect(settings.single.entryPath, '/');
    expect(settings.single.version, 7);
    expect(settings.single.activeOrigin, 'http://127.0.0.1:52713/');
  });

  testWidgets('World settings render as a standalone surface', (tester) async {
    const snapshot = WorldSettingsSnapshot(
      key: 'issues',
      name: 'Issues',
      worldMount: 'issues',
      entryPath: '/',
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
    expect(find.text('v7'), findsOneWidget);
    expect(find.text('http://127.0.0.1:52713/'), findsOneWidget);
  });

  testWidgets('World settings own an Astrolabe window shell', (tester) async {
    const snapshot = WorldSettingsSnapshot(
      key: 'issues',
      name: 'Issues',
      worldMount: 'issues',
      entryPath: '/',
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
      key: 'issues',
      name: 'Issues',
      worldMount: 'issues',
      entryPath: '/',
      version: 7,
      activeOrigin: 'http://127.0.0.1:52713/',
      dark: true,
    );

    final decoded = WorldSettingsSnapshot.fromArguments([
      '--unrelated',
      snapshot.toArgument(),
    ]);
    expect(decoded?.key, snapshot.key);
    expect(decoded?.worldMount, snapshot.worldMount);
    expect(decoded?.activeOrigin, snapshot.activeOrigin);
    expect(decoded?.dark, isTrue);
  });

  testWidgets('Library search filters the passive rail', (tester) async {
    final asked = await _pump(
      tester,
      _view(
        library: [
          _row(mount: 'issues', name: 'IssueWorld'),
          _row(mount: 'notes', name: 'Notes'),
        ],
      ),
    );

    await tester.enterText(find.byKey(const ValueKey('library-search')), 'notes');
    await tester.pump();

    expect(find.text('IssueWorld'), findsNothing);
    expect(find.text('Notes'), findsWidgets);
    expect(
      asked,
      isEmpty,
      reason: 'filtering the Library placed or opened a World',
    );
  });

  test('a .lait store names the project beside it', () {
    expect(projectDirectory(r'D:\work\foo\.lait'), r'D:\work\foo');
    expect(projectDirectory(r'D:/work/foo/.lait'), r'D:/work/foo');
    expect(projectDirectory(r'D:\work\foo'), isNull);
    expect(projectDirectory(null), isNull);
  });

  testWidgets('writing an agent binding pins the selected World',
      (tester) async {
    tester.view.physicalSize = const Size(1040, 1600);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    final asked = await _pump(
      tester,
      _view(library: [_row(name: 'Issues')]),
    );

    expect(find.byKey(const ValueKey('library-agent-binding')), findsOneWidget);
    await tester.enterText(
      find.byKey(const ValueKey('library-agent-project')),
      r'D:\work\tracker',
    );
    await tester.pump();
    await tester.tap(find.text('Write binding'));
    await tester.pump();

    expect(asked, hasLength(1));
    expect(
      asked.single,
      const ActionRequest.installMcp(
        client: 'claude',
        name: 'lait-issues',
        noAgent: false,
        project: r'D:\work\tracker',
        world: 'issues',
        preview: false,
      ),
      reason: 'the binding did not pin the World this row is',
    );
  });
}
