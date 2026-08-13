/// The book window, canned: press a real control, read the ActionRequest.
library;

import 'package:astrolabe/src/core/client.dart';
import 'package:astrolabe/src/shell/book.dart';
import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/material.dart' show MaterialApp, Scaffold, ThemeMode;
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:astrolabe/src/shell/theme.dart';

ClientView _view({
  BookFacts? book,
  List<String> inFlight = const [],
}) =>
    ClientView(
      loading: false,
      library_: const [],
      heads: const [],
      devices: const [],
      storage: const [],
      orbits: const [],
      book: book,
      notices: const [],
      failures: const [],
      inFlight: inFlight,
    );

BookFacts _book(List<CardRow> cards, {int pending = 0}) => BookFacts(
      cards: cards,
      migrationComplete: pending == 0,
      migrationPending: pending,
      migrationImported: 0,
    );

CardRow _card({
  String id = 'crd_one',
  String name = 'Ada',
  String note = '',
  List<String> handles = const [],
  bool self = false,
}) =>
    CardRow(
      card: id,
      name: name,
      note: note,
      handles: handles,
      groups: const [],
      selfClaim: self,
    );

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
            child: BookPage(
              themeMode: ThemeMode.dark,
              onToggleTheme: _noop,
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
  testWidgets('refresh on the book asks the core, not a second model',
      (tester) async {
    final asked = await _pump(tester, _view());
    await tester.tap(find.bySemanticsLabel('Refresh'));
    expect(asked, [const ActionRequest.refresh()]);
  });

  testWidgets('an in-flight refresh disables the control', (tester) async {
    final asked = await _pump(
      tester,
      _view(inFlight: [ActionKeys.refresh]),
    );
    await tester.tap(
      find.bySemanticsLabel('Refresh'),
      warnIfMissed: false,
    );
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

  testWidgets('saving a new card asks the core for a put, not a local write',
      (tester) async {
    final asked = await _pump(tester, _view(book: _book(const [])));
    await tester.tap(find.text('New card'));
    await tester.pump(const Duration(milliseconds: 300));

    await tester.enterText(find.byKey(const ValueKey('book-name')), 'Ada');
    await tester.enterText(find.byKey(const ValueKey('book-note')), 'colleague');
    await tester.ensureVisible(find.text('Save'));
    await tester.tap(find.text('Save'));
    await tester.pump();

    expect(
      asked,
      [
        const ActionRequest.bookPut(
          card: null,
          name: 'Ada',
          note: 'colleague',
        ),
      ],
    );
  });

  testWidgets('claiming My Card names the card, never a name', (tester) async {
    final asked = await _pump(
      tester,
      _view(book: _book([_card()])),
    );
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
    await tester.tap(find.text('Claim as My Card'), warnIfMissed: false);
    expect(asked, isEmpty);
  });

  testWidgets('delete stays disabled until the card name is typed',
      (tester) async {
    final asked = await _pump(
      tester,
      _view(book: _book([_card(name: 'Ada')])),
    );
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

  testWidgets('search is a draft: it filters, it does not dispatch',
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
    await tester.enterText(find.byType(Input), 'grace');
    await tester.pump();

    expect(find.text('Ada'), findsNothing);
    expect(find.text('Grace'), findsOneWidget);
    expect(asked, isEmpty, reason: 'searching the book asked the core');
  });

  testWidgets('export asks the core for a path, not a second model',
      (tester) async {
    final asked = await _pump(tester, _view(book: _book([_card()])));
    await tester.tap(find.text('Export'));
    await tester.pump(const Duration(milliseconds: 300));
    await tester.enterText(
      find.byKey(const ValueKey('book-bundle-path')),
      r'D:\tmp\cards.json',
    );
    await tester.ensureVisible(find.text('Export').last);
    await tester.tap(find.text('Export').last);
    await tester.pump();
    expect(
      asked,
      [const ActionRequest.bookExport(path: r'D:\tmp\cards.json')],
    );
  });

  testWidgets('unlinking a handle names the card and the handle',
      (tester) async {
    final asked = await _pump(
      tester,
      _view(
        book: _book([
          _card(handles: const ['actor:ws_one:act_ada']),
        ]),
      ),
    );
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
}

void _noop() {}
