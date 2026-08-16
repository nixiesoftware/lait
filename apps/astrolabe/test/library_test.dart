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

import 'dart:convert' show base64Decode;
import 'dart:typed_data' show Uint8List;

import 'package:astrolabe/src/core/client.dart';
import 'package:astrolabe/src/settings/window.dart';
import 'package:astrolabe/src/shell/person.dart';
import 'package:astrolabe/src/surfaces/library.dart';
// `Image` hidden for the reason the surface hides it: covalence's is a network
// component, and the artwork under test is bytes.
import 'package:covalence/covalence.dart' hide Surface, Image;
import 'package:flutter/material.dart' show MaterialApp, Scaffold;
import 'package:flutter/widgets.dart';
import 'package:lit_ui/lit_ui.dart' show Lit;
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

/// A real 1×1 PNG. `Image.memory` runs its bytes through the actual decoder,
/// so a fixture that only looks like an image fails there rather than in the
/// assertion it was written for.
final Uint8List _png = base64Decode(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQ'
  'GAhKmMIQAAAABJRU5ErkJggg==',
);

Future<List<ActionRequest>> _pump(
  WidgetTester tester,
  ClientView view, {
  OpenWorldSettings? onSettings,
  Map<String, WorldArtwork> artwork = const {},
}) async {
  final asked = <ActionRequest>[];
  await tester.pumpWidget(
    MaterialApp(
      theme: covalenceTheme(const ThemeConfig()),
      home: ClientScope(
        client:
            Client.canned(view, onDispatch: asked.add, artwork: artwork),
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

Future<void> _pumpLibraryPage(
  WidgetTester tester,
  ClientView view, {
  Map<String, WorldArtwork> artwork = const {},
}) async {
  tester.view.physicalSize = const Size(1040, 720);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);

  await tester.pumpWidget(
    MaterialApp(
      theme: covalenceTheme(const ThemeConfig()),
      home: ClientScope(
        client: Client.canned(view, artwork: artwork),
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
    // The reference client's anatomy and its colour: launch is the one solid
    // green slab. Green belongs to the act that starts a World — the running
    // control wears the white stop coat — so a slip reads before a label.
    final launchSlab = tester.widget<Lit>(
      find.ancestor(
        of: find.text('LAUNCH'),
        matching: find.byType(Lit),
      ),
    );
    expect(launchSlab.baseColor, kLaunchSlabFill);
    expect(
      launchSlab.baseColor,
      isNot(kStopSlabFill),
      reason: 'the two acts share one coat',
    );
    // The two coats are inverses: white ink on the green, dark ink on the
    // white stop slab. Asserted against the launch constant rather than a
    // literal, so the pair cannot drift apart in one place only.
    expect(
      tester.widget<Text>(find.text('LAUNCH')).style?.color,
      kLaunchSlabInk,
    );
    expect(
      tester.widget<Icon>(find.byIcon(AppIcons.playArrow)).color,
      kLaunchSlabInk,
      reason: 'the play mark and its word disagree about the ink',
    );
    expect(asked, hasLength(1));
    expect(
      asked.single,
      const ActionRequest.open(entryPath: '/'),
      reason: 'opening asked for somewhere other than the declared entry',
    );
  });

  testWidgets('the action slabs are content-sized, not band-wide',
      (tester) async {
    // A Container handed an `alignment` expands to its constraints rather than
    // its child, and these slabs sit in a Wrap that offers the whole band —
    // which is how LAUNCH once stretched the full width of the pane. Measured
    // against the band rather than a magic number, so the guard survives a
    // change of window size.
    await _pumpLibraryPage(tester, _view(library: [_row()]));

    final band = find.byKey(const ValueKey('library-open-band'));
    final launch = find.ancestor(
      of: find.text('LAUNCH'),
      matching: find.byType(Lit),
    );
    expect(
      tester.getSize(launch).width,
      lessThan(tester.getSize(band).width / 2),
      reason: 'the launch slab stretched to fill the action band',
    );

    await _pumpLibraryPage(
      tester,
      _view(library: [_row()], heads: const [_identityHead]),
    );
    for (final slab in find
        .descendant(of: band, matching: find.byType(Lit))
        .evaluate()
        .map((element) => find.byWidget(element.widget))) {
      expect(
        tester.getSize(slab).width,
        lessThan(tester.getSize(band).width / 2),
        reason: 'a segment of the stop slab filled the action band',
      );
    }
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

  testWidgets('a head address is not the front page\'s to state',
      (tester) async {
    await _pump(
      tester,
      _view(
        library: [_row()],
        heads: const [_identityHead],
      ),
    );

    // The address left this surface with the SERVING NOW panel. The footer's
    // launch notice and World settings carry it, both credential-stripped;
    // what must never appear here is the credential itself.
    expect(find.text('http://127.0.0.1:52713/'), findsNothing);
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

  testWidgets('a serving head is one solid split control — STOP, then Go to',
      (tester) async {
    // "Running" is the identity head's own liveness: the destination is up,
    // and Open is a handoff rather than a start. The act offered against it
    // is STOP — the head is what this client started, so it is what this
    // client can end.
    final asked = await _pump(
      tester,
      _view(
        library: [_row(opensAt: '/issues')],
        heads: const [_identityHead],
      ),
    );

    expect(find.text('Go to'), findsNothing);
    expect(find.text('Cancel'), findsNothing);

    final openBand = find.byKey(const ValueKey('library-open-band'));
    // The state chip said RUNNING here once. The reference client's running
    // control is the act; the state lives in the badge and the rail.
    expect(
      find.descendant(of: openBand, matching: find.text('RUNNING')),
      findsNothing,
    );
    final stopLabel = find.descendant(
      of: openBand,
      matching: find.text('STOP'),
    );
    expect(stopLabel, findsOneWidget);
    expect(tester.widget<Text>(stopLabel).style?.fontSize, 20);
    expect(tester.widget<Text>(stopLabel).style?.fontWeight, FontWeight.w400);

    // The reference client's anatomy: one solid slab — a stop mark and STOP,
    // a hairline, the handoff glyph — not a status chip beside a detached
    // ghost button. And its colour: the white stop coat, never launch's green.
    final stopMark = find.descendant(
      of: openBand,
      matching: find.byIcon(AppIcons.close),
    );
    expect(stopMark, findsOneWidget);
    expect(tester.widget<Icon>(stopMark).size, 20);
    expect(
      find.descendant(of: openBand, matching: find.byIcon(AppIcons.playArrow)),
      findsNothing,
      reason: 'a running World offered the start glyph',
    );
    for (final slab in tester.widgetList<Lit>(
      find.descendant(of: openBand, matching: find.byType(Lit)),
    )) {
      expect(
        slab.baseColor,
        kStopSlabFill,
        reason: 'a segment of the stop slab is not the white stop coat',
      );
      expect(
        slab.baseColor,
        isNot(kLaunchSlabFill),
        reason: 'the stop slab wears the colour that launches',
      );
    }
    // Dark ink on the white slab, in either theme — a text rung would flip
    // with polarity and put white on white.
    expect(tester.widget<Text>(stopLabel).style?.color, kStopSlabInk);
    expect(
      tester.widget<Icon>(stopMark).color,
      kStopSlabInk,
      reason: 'the stop mark and its word disagree about the ink',
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
      tester.widget<Icon>(stopMark).size,
      reason: 'the slab\'s two glyphs share one size',
    );
    expect(tester.getCenter(handoff).dy, tester.getCenter(stopLabel).dy);

    // The halves are different acts: STOP ends the head this client owns;
    // the handoff goes to it at the World-declared entry path.
    await tester.tap(stopLabel);
    await tester.pump();
    await tester.tap(handoff);
    await tester.pump();
    expect(
      asked,
      const [
        ActionRequest.stopHead(id: 'identity:default'),
        ActionRequest.open(entryPath: '/issues'),
      ],
      reason: 'a segment asked for something other than its own act',
    );
  });

  testWidgets('a head this client does not own is not one it may stop',
      (tester) async {
    // Ownership is the boundary the supervisor enforces, so the surface must
    // not draw past it. Somebody ran `lait` themselves: the World is reachable
    // and Open still works, but STOP is absent rather than present-and-refused.
    final asked = await _pump(
      tester,
      _view(
        library: [_row(opensAt: '/issues')],
        heads: const [
          HeadRow(
            id: 'identity:external',
            kind: 'browser',
            origin: 'http://127.0.0.1:7717/',
            owned: false,
          ),
        ],
      ),
    );

    final openBand = find.byKey(const ValueKey('library-open-band'));
    expect(
      find.descendant(of: openBand, matching: find.text('STOP')),
      findsNothing,
      reason: 'the client offered to stop a head it never started',
    );
    // The handoff is still the whole control, and still names itself.
    final handoff = find.descendant(
      of: openBand,
      matching: find.text('OPEN'),
    );
    expect(handoff, findsOneWidget);

    await tester.tap(handoff);
    await tester.pump();
    expect(asked, const [ActionRequest.open(entryPath: '/issues')]);
  });

  testWidgets('an in-flight stop is visibly stopping, with no second press',
      (tester) async {
    final asked = await _pump(
      tester,
      _view(
        library: [_row(opensAt: '/issues')],
        heads: const [_identityHead],
        inFlight: const ['head.stop:identity:default'],
      ),
    );

    expect(find.text('STOPPING'), findsOneWidget);
    expect(
      find.text('STOP'),
      findsNothing,
      reason: 'the control stayed pressable while its own stop was in flight',
    );
    expect(asked, isEmpty);
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

    // The act in flight wears the stop coat, not a translucent pill: the band
    // must not change weight or shape under a state that lasts a second.
    final pending = tester.widget<Lit>(
      find.ancestor(
        of: find.text('LAUNCHING'),
        matching: find.byType(Lit),
      ),
    );
    expect(pending.baseColor, kStopSlabFill);
    expect(
      tester.widget<Text>(find.text('LAUNCHING')).style?.color,
      kStopSlabInk,
    );
    expect(
      tester.widget<Text>(find.text('LAUNCHING')).style?.fontSize,
      20,
      reason: 'the pending label is not the slab type size',
    );
    expect(tester.widget<Progress>(find.byType(Progress)).size,
        ProgressSize.lg);
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

    // The action band carries the act and nothing else. The version is still
    // known — it rides the settings snapshot below — but it is a fact about
    // the World rather than something to do with it, so it is not drawn
    // beside the control.
    expect(
      find.text('v7'),
      findsNothing,
      reason: 'the action band grew a readout beside its one control',
    );
    expect(find.text('VERSION'), findsNothing);

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

  testWidgets('the rail draws every installed World, unsearched and unlabelled',
      (tester) async {
    // The rail is the install list: a handful of rows compiled into the
    // build. There is no search field to narrow it, and the ordinary state
    // carries no heading — READY over a row that is ready says nothing the
    // row does not.
    final asked = await _pump(
      tester,
      _view(
        library: [
          _row(mount: 'issues', name: 'IssueWorld'),
          _row(mount: 'notes', name: 'Notes'),
        ],
      ),
    );

    expect(find.byKey(const ValueKey('library-search')), findsNothing);
    expect(find.text('READY'), findsNothing);
    final rail = find.byKey(const ValueKey('library-rail'));
    expect(find.descendant(of: rail, matching: find.text('IssueWorld')),
        findsOneWidget);
    expect(
        find.descendant(of: rail, matching: find.text('Notes')), findsOneWidget);
    expect(
      asked,
      isEmpty,
      reason: 'drawing the Library placed or opened a World',
    );
  });

  testWidgets('a World that ships artwork is drawn from it', (tester) async {
    await _pumpLibraryPage(
      tester,
      _view(library: [_row(mount: 'issues', name: 'Issues')]),
      artwork: {'issues': WorldArtwork(mark: _png, hero: _png)},
    );

    final rail = find.byKey(const ValueKey('library-rail'));
    final hero = find.byKey(const ValueKey('library-hero'));
    expect(
      find.descendant(of: rail, matching: find.byType(Image)),
      findsOneWidget,
    );
    expect(
      find.descendant(of: hero, matching: find.byType(Image)),
      findsOneWidget,
    );
    // The derived plate is what artwork replaces, not something it is drawn
    // over: a mark and a letter in the same square is the letter showing
    // through wherever the art is transparent.
    expect(find.descendant(of: rail, matching: find.text('I')), findsNothing);
  });

  testWidgets('a World that ships none keeps the plate cut from its accent',
      (tester) async {
    // The default, and deliberately: every World was drawn this way before any
    // of them shipped art, so shipping none is a choice rather than a gap.
    await _pumpLibraryPage(
      tester,
      _view(library: [_row(mount: 'issues', name: 'Issues')]),
    );

    final rail = find.byKey(const ValueKey('library-rail'));
    final hero = find.byKey(const ValueKey('library-hero'));
    expect(find.descendant(of: rail, matching: find.text('I')), findsOneWidget);
    expect(find.descendant(of: rail, matching: find.byType(Image)), findsNothing);
    expect(find.descendant(of: hero, matching: find.byType(Image)), findsNothing);
  });

  testWidgets('the Library detail offers no agent-binding authoring',
      (tester) async {
    // The section is hidden: the MCP install capability stays in the core,
    // but the Library's detail column no longer authors bindings.
    final asked = await _pump(
      tester,
      _view(library: [_row(name: 'Issues')]),
    );

    expect(find.byKey(const ValueKey('library-agent-binding')), findsNothing);
    expect(find.text('AGENT BINDING'), findsNothing);
    expect(find.text('Write binding'), findsNothing);
    expect(asked, isEmpty);
  });
}
