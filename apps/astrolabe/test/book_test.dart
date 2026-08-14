/// The book window, canned: press a real control, read the ActionRequest.
library;

import 'package:astrolabe/src/core/client.dart';
import 'package:astrolabe/src/shell/book.dart';
import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/material.dart' show MaterialApp, Scaffold;
import 'package:flutter/services.dart' show LogicalKeyboardKey;
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:astrolabe/src/shell/record.dart';
import 'package:astrolabe/src/shell/theme.dart';
import 'package:astrolabe/src/shell/window.dart';

ClientView _view({
  BookFacts? book,
  List<String> inFlight = const [],
  bool hostAnswered = false,
}) =>
    ClientView(
      loading: false,
      library_: const [],
      host: hostAnswered
          ? const HostFacts(
              version: 'test',
              identityHome: '',
              spacesRoot: '',
              orbitCount: 0,
            )
          : null,
      heads: const [],
      devices: const [],
      storage: const [],
      orbits: const [],
      book: book,
      notices: const [],
      failures: const [],
      inFlight: inFlight,
    );

BookFacts _book(
  List<CardRow> cards, {
  int pending = 0,
  List<SuggestionRow> suggestions = const [],
}) =>
    BookFacts(
      cards: cards,
      migrationComplete: pending == 0,
      migrationPending: pending,
      migrationImported: 0,
      suggestions: suggestions,
    );

SuggestionRow _suggestion({
  String id = 'sug_abc123',
  String name = 'Grace',
  String note = '',
  List<String> handles = const [],
}) =>
    SuggestionRow(
      suggestion: id,
      name: name,
      note: note,
      handles: handles,
    );

CardRow _card({
  String id = 'crd_one',
  String name = 'Ada',
  String note = '',
  List<String> addresses = const [],
  List<String> devices = const [],
  List<String> agents = const [],
  List<String> groups = const [],
  String? picture,
  bool self = false,
  PresenceView? presence,
}) =>
    CardRow(
      card: id,
      name: name,
      note: note,
      handles: [...addresses, ...devices, ...agents],
      addresses: addresses,
      devices: devices,
      agents: agents,
      picture: picture,
      groups: groups,
      selfClaim: self,
      presence: presence,
    );

/// Double-click a person's row: the gesture that opens the profile
/// subsurface in the parent window.
Future<void> _openProfile(WidgetTester tester, String name) async {
  await tester.tap(find.text(name).first);
  await tester.pump(const Duration(milliseconds: 80));
  await tester.tap(find.text(name).first);
  await tester.pump(const Duration(milliseconds: 400));
}

Future<List<ActionRequest>> _pump(
  WidgetTester tester,
  ClientView view,
) async {
  tester.view.physicalSize = const Size(1200, 2000);
  tester.view.devicePixelRatio = 1;
  await tester.binding.setSurfaceSize(const Size(1200, 2000));
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  addTearDown(() => tester.binding.setSurfaceSize(null));

  final asked = <ActionRequest>[];
  await tester.pumpWidget(
    MaterialApp(
      theme: astrolabeTheme(Brightness.light),
      home: ClientScope(
        client: Client.canned(view, onDispatch: asked.add),
        child: const Scaffold(
          body: Padding(
            padding: EdgeInsets.all(16),
            child: BookPage(),
          ),
        ),
      ),
    ),
  );
  await tester.pump(const Duration(milliseconds: 300));
  return asked;
}

void main() {
  testWidgets('the chrome carries no name; the card is the identity',
      (tester) async {
    await tester.pumpWidget(BookApp(client: Client.canned(_view())));
    await tester.pump(const Duration(milliseconds: 100));

    expect(find.text('ASTROLABE'), findsNothing);
    expect(find.text('Address book'), findsNothing);
    // No caption band at all: the canonical card holds the window's top and
    // the two controls float beside it, with no rule between them.
    expect(find.byType(WindowChrome), findsNothing);
    expect(find.bySemanticsLabel('Minimise'), findsOneWidget);
    expect(find.bySemanticsLabel('Close'), findsOneWidget);
    // And no operational metrics row — that record lives on the main window.
    expect(find.byType(OperationalBar), findsNothing);
  });

  testWidgets('My Card is the canonical card: a face, a name, a status',
      (tester) async {
    await _pump(
      tester,
      _view(
        book: _book([_card(name: 'Ada', self: true)]),
        hostAnswered: true,
      ),
    );
    // The default face is a boxed monogram, drawn wherever the card is — the
    // canonical card up top and its phone-book row alike.
    expect(find.text('A'), findsNWidgets(2));
    // The canonical card's Online is the local fact — this identity's daemon
    // answered the read. The list row measures nothing here and says nothing.
    expect(find.text('Online'), findsOneWidget);
    // The claimed name appears on the card and on its list row alike.
    expect(find.text('Ada'), findsNWidgets(2));
  });

  testWidgets('an unreachable daemon takes My Card\'s Online with it',
      (tester) async {
    await _pump(
      tester,
      _view(book: _book([_card(name: 'Ada', self: true)])),
    );
    expect(find.text('Online'), findsNothing);
  });

  testWidgets('a row wears measured presence over the authored note',
      (tester) async {
    await _pump(
      tester,
      _view(
        book: _book([
          _card(
            id: 'crd_one',
            name: 'Ada',
            note: 'Met at the workshop',
            presence: PresenceView.online,
          ),
          _card(id: 'crd_two', name: 'Grace', presence: PresenceView.offline),
          _card(id: 'crd_three', name: 'Basalt', presence: PresenceView.away),
        ]),
      ),
    );
    // Presence is the fact a friends list is for; the note yields to it and
    // stays on the profile page.
    expect(find.text('Online'), findsOneWidget);
    expect(find.text('Met at the workshop'), findsNothing);
    // Offline and Away are measurements too, and they draw — the measured
    // absence twice, as its section head and as the row's own status.
    expect(find.text('Offline'), findsNWidgets(2));
    expect(find.text('Away'), findsOneWidget);
  });

  testWidgets('a row states the authored note and invents nothing',
      (tester) async {
    await _pump(
      tester,
      _view(
        book: _book([
          _card(id: 'crd_one', name: 'Ada', note: 'Met at the workshop'),
          _card(id: 'crd_two', name: 'Grace'),
        ]),
      ),
    );
    expect(find.text('Met at the workshop'), findsOneWidget);
    // No My Card is claimed and nothing was measured, so nothing on this
    // surface says Online — an unmeasured presence is absent, never a
    // default.
    expect(find.text('Online'), findsNothing);
  });

  testWidgets('the list is headed Contacts, with search beside it',
      (tester) async {
    await _pump(tester, _view(book: _book([_card()])));
    expect(find.text('CONTACTS'), findsOneWidget);
    expect(find.bySemanticsLabel('Search cards'), findsOneWidget);
  });

  testWidgets('the rule parts the measured absence from everyone else',
      (tester) async {
    await _pump(
      tester,
      _view(
        book: _book([
          // Listed offline-first so the rule, not arrival order, does the
          // parting — and the agent sits with everyone else: what an
          // identity is and whether it is here are different axes.
          _card(
            id: 'crd_moon',
            name: 'Moon',
            addresses: const ['actor:ws_one:act_moon'],
            presence: PresenceView.offline,
          ),
          _card(
            id: 'crd_claude',
            name: 'claude',
            addresses: const ['actor:ws_one:act_claude'],
            groups: const ['Agents'],
            presence: PresenceView.online,
          ),
          _card(
            id: 'crd_ada',
            name: 'Ada',
            addresses: const ['actor:ws_one:act_ada'],
          ),
        ]),
      ),
    );
    expect(find.byType(Separator), findsOneWidget);
    final rule = tester.getTopLeft(find.byType(Separator));
    final online = tester.getTopLeft(find.text('claude'));
    final unmeasured = tester.getTopLeft(find.text('Ada'));
    final offline = tester.getTopLeft(find.text('Moon'));
    expect(
      online.dy,
      lessThan(unmeasured.dy),
      reason: 'measured presence sorts above the unmeasured',
    );
    expect(unmeasured.dy, lessThan(rule.dy));
    expect(rule.dy, lessThan(offline.dy));
    // The head and Moon's own status line both state the measured absence.
    // The count sits on the present section alone — the absent get no tally.
    expect(find.text('Offline'), findsNWidgets(2));
    expect(find.text('(2)'), findsOneWidget);
    expect(find.text('(1)'), findsNothing);
    // The grey-out: the offline face is dimmed to the offline register.
    final opacities = tester
        .widgetList<Opacity>(find.byType(Opacity))
        .map((widget) => widget.opacity);
    expect(opacities, contains(0.45));
  });

  testWidgets('a book with nobody measured offline draws no rule',
      (tester) async {
    await _pump(
      tester,
      _view(
        book: _book([
          _card(id: 'crd_one', name: 'Ada', presence: PresenceView.online),
          // Unmeasured is not a lesser offline: Grace stays up top.
          _card(id: 'crd_two', name: 'Grace'),
        ]),
      ),
    );
    expect(find.byType(Separator), findsNothing);
    expect(find.text('Offline'), findsNothing);
    expect(find.text('Contacts'), findsOneWidget);
  });

  testWidgets('an agent wears the AI mark, never a section of its own',
      (tester) async {
    await _pump(
      tester,
      _view(
        book: _book([
          _card(
            id: 'crd_claude',
            name: 'claude',
            addresses: const ['actor:ws_one:act_claude'],
            groups: const ['Agents'],
          ),
          _card(id: 'crd_grok', name: 'grok', agents: const ['agent:h1:grok']),
          _card(
            id: 'crd_ada',
            name: 'Ada',
            addresses: const ['actor:ws_one:act_ada'],
          ),
        ]),
      ),
    );
    // Both agent spellings wear the mark; the person does not.
    expect(find.byType(AiMark), findsNWidgets(2));
    // Nobody is measured offline, so kind alone parts nothing.
    expect(find.byType(Separator), findsNothing);
  });

  testWidgets('the book window is portrait for good and offers no maximise',
      (tester) async {
    await tester.pumpWidget(BookApp(client: Client.canned(_view())));
    await tester.pump(const Duration(milliseconds: 100));

    final frame = tester.widget<AstrolabeWindowFrame>(
      find.byType(AstrolabeWindowFrame),
    );
    expect(frame.maximisable, isFalse);

    // The portrait rule is arithmetic, not behavioral: the widest width the
    // native config may grant is no larger than the shortest height, so no
    // drag, snap, or shortcut can produce a landscape book.
    final config = frame.ownedConfiguration!;
    expect(config.maximisable, isFalse);
    expect(config.maximumWidth, isNotNull);
    expect(
      config.maximumWidth!,
      lessThanOrEqualTo(config.minimumSize.height),
      reason: 'a width ceiling above the height floor readmits landscape',
    );
    expect(config.size.width, lessThan(config.size.height));
    expect(config.size.width, lessThanOrEqualTo(config.maximumWidth!));
    expect(config.minimumSize.width, lessThanOrEqualTo(config.maximumWidth!));

    // No maximise affordance is drawn — absence, not a disabled button.
    expect(find.bySemanticsLabel('Maximise'), findsNothing);
    expect(find.bySemanticsLabel('Restore'), findsNothing);
    expect(find.bySemanticsLabel('Minimise'), findsOneWidget);
    expect(find.bySemanticsLabel('Close'), findsOneWidget);
  });

  testWidgets('refresh on the book asks the core, not a second model',
      (tester) async {
    final asked = await _pump(tester, _view());
    await tester.sendKeyEvent(LogicalKeyboardKey.f5);
    expect(asked, [const ActionRequest.refresh()]);
  });

  testWidgets('an in-flight refresh keeps the shortcut quiet', (tester) async {
    final asked = await _pump(
      tester,
      _view(inFlight: [ActionKeys.refresh]),
    );
    await tester.sendKeyEvent(LogicalKeyboardKey.f5);
    expect(asked, isEmpty);
  });

  testWidgets('unread and empty are told apart on the book', (tester) async {
    await _pump(tester, _view());
    expect(find.text('The book has not been read.'), findsOneWidget);
    expect(find.text('No cards.'), findsNothing);

    await _pump(tester, _view(book: _book(const [])));
    expect(find.text('The book has not been read.'), findsNothing);
    expect(find.text('No cards.'), findsOneWidget);
    expect(find.text('No My Card.'), findsOneWidget);
  });

  testWidgets('a row is minimal; the profile subsurface carries the actions',
      (tester) async {
    await _pump(
      tester,
      _view(book: _book([_card(addresses: const ['actor:ws_one:act_ada'])])),
    );
    // The list shows the person, never the machinery.
    expect(find.text('Edit'), findsNothing);
    expect(find.text('Delete'), findsNothing);
    expect(find.text('ADDRESSES'), findsNothing);

    await _openProfile(tester, 'Ada');
    expect(find.text('ADDRESSES'), findsOneWidget);
    expect(find.text('Edit'), findsOneWidget);
    expect(find.text('Delete'), findsOneWidget);
    expect(find.bySemanticsLabel('Back to the book'), findsOneWidget);

    // Escape peels the profile and the list is back.
    await tester.sendKeyEvent(LogicalKeyboardKey.escape);
    await tester.pump();
    expect(find.text('Edit'), findsNothing);
    expect(find.text('ADDRESSES'), findsNothing);
  });

  testWidgets('claiming My Card names the card, never a name', (tester) async {
    final asked = await _pump(
      tester,
      _view(book: _book([_card()])),
    );
    await _openProfile(tester, 'Ada');
    await tester.tap(find.text('Claim as My Card'));
    expect(
      asked,
      [const ActionRequest.bookClaimSelf(card: 'crd_one')],
    );
  });

  testWidgets('an in-flight claim disables the control', (tester) async {
    final asked = await _pump(
      tester,
      _view(
        book: _book([_card()]),
        inFlight: [ActionKeys.bookClaim('crd_one')],
      ),
    );
    await _openProfile(tester, 'Ada');
    await tester.tap(find.text('Claim as My Card'), warnIfMissed: false);
    expect(asked, isEmpty);
  });

  testWidgets('delete stays disabled until the card name is typed',
      (tester) async {
    final asked = await _pump(
      tester,
      _view(book: _book([_card(name: 'Ada')])),
    );
    await _openProfile(tester, 'Ada');
    await tester.ensureVisible(find.text('Delete'));
    await tester.tap(find.text('Delete'));
    await tester.pump(const Duration(milliseconds: 300));

    await tester.tap(find.text('Delete').last, warnIfMissed: false);
    await tester.pump();
    expect(asked, isEmpty, reason: 'delete fired without typing the name');

    await tester.enterText(find.byKey(const ValueKey('book-confirm-name')), 'Ada');
    await tester.pump();
    await tester.ensureVisible(find.text('Delete').last);
    await tester.tap(find.text('Delete').last);
    await tester.pump();
    expect(asked, [const ActionRequest.bookDelete(card: 'crd_one')]);
  });

  testWidgets('search is a button first, and its draft only filters',
      (tester) async {
    final asked = await _pump(
      tester,
      _view(
        book: _book([
          _card(id: 'crd_one', name: 'Ada'),
          _card(id: 'crd_two', name: 'Grace'),
        ]),
      ),
    );
    expect(find.byType(Input), findsNothing);

    await tester.tap(find.bySemanticsLabel('Search cards'));
    await tester.pump();
    await tester.enterText(find.byType(Input), 'grace');
    await tester.pump();

    expect(find.text('Ada'), findsNothing);
    expect(find.text('Grace'), findsOneWidget);
    expect(asked, isEmpty, reason: 'searching the book asked the core');

    // Cancelling closes the field and clears the draft — the full list is
    // back and nothing was dispatched.
    await tester.tap(find.bySemanticsLabel('Cancel search'));
    await tester.pump();
    expect(find.byType(Input), findsNothing);
    expect(find.text('Ada'), findsOneWidget);
    expect(find.text('Grace'), findsOneWidget);
    expect(asked, isEmpty, reason: 'cancelling the search asked the core');
  });

  testWidgets('unlinking a handle names the card and the handle',
      (tester) async {
    final asked = await _pump(
      tester,
      _view(
        book: _book([
          _card(addresses: const ['actor:ws_one:act_ada']),
        ]),
      ),
    );
    await _openProfile(tester, 'Ada');
    await tester.tap(find.bySemanticsLabel('Unlink actor:ws_one:act_ada'));
    expect(
      asked,
      [
        const ActionRequest.bookUnlink(
          card: 'crd_one',
          handle: 'actor:ws_one:act_ada',
        ),
      ],
    );
  });
  testWidgets('a staged suggestion is drawn apart from the book', (tester) async {
    final view = _view(
      book: _book(
        [_card()],
        suggestions: [_suggestion(name: 'Grace', handles: const ['actor:ws_x:act_y'])],
      ),
    );
    await _pump(tester, view);
    expect(find.text('1 suggested from files'), findsOneWidget);
    expect(find.text('Grace'), findsOneWidget);
    expect(find.text('Accept'), findsOneWidget);
    expect(find.text('Dismiss'), findsOneWidget);
  });

  testWidgets('accepting a suggestion asks the core by id, never by name',
      (tester) async {
    final view = _view(book: _book(const [], suggestions: [_suggestion()]));
    final asked = await _pump(tester, view);
    await tester.tap(find.text('Accept'));
    await tester.pump();
    expect(
      asked.single,
      const ActionRequest.bookAccept(suggestion: 'sug_abc123'),
    );
  });

  testWidgets('dismissing a suggestion asks the core and touches no card',
      (tester) async {
    final view = _view(book: _book(const [], suggestions: [_suggestion()]));
    final asked = await _pump(tester, view);
    await tester.tap(find.text('Dismiss'));
    await tester.pump();
    expect(
      asked.single,
      const ActionRequest.bookDismiss(suggestion: 'sug_abc123'),
    );
  });

  testWidgets('an in-flight accept disables its own control only',
      (tester) async {
    final view = _view(
      book: _book(const [], suggestions: [_suggestion()]),
      inFlight: [ActionKeys.bookAccept('sug_abc123')],
    );
    await _pump(tester, view);
    final accept = tester.widget<Button>(
      find.widgetWithText(Button, 'Accept'),
    );
    final dismiss = tester.widget<Button>(
      find.widgetWithText(Button, 'Dismiss'),
    );
    expect(accept.onPressed, isNull);
    expect(dismiss.onPressed, isNotNull);
  });

}
