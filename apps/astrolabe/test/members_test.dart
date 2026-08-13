/// Members decorate from the book; they never treat a name as an id.
library;

import 'package:astrolabe/src/core/client.dart';
import 'package:astrolabe/src/surfaces/members.dart';
import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/material.dart' show MaterialApp, Scaffold;
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

ClientView _view({SpaceRow? space}) => ClientView(
      loading: false,
      library_: const [],
      heads: const [],
      devices: const [],
      storage: const [],
      orbits: const [],
      space: space,
      notices: const [],
      failures: const [],
      inFlight: const [],
    );

Future<void> _pump(WidgetTester tester, ClientView view) async {
  tester.view.physicalSize = const Size(1040, 900);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);

  await tester.pumpWidget(
    MaterialApp(
      theme: covalenceTheme(const ThemeConfig()),
      home: ClientScope(
        client: Client.canned(view),
        child: const Scaffold(
          body: Padding(
            padding: EdgeInsets.all(16),
            child: MembersSurface(),
          ),
        ),
      ),
    ),
  );
  await tester.pump(const Duration(milliseconds: 300));
}

void main() {
  testWidgets('an authored card name is drawn above the Space nick',
      (tester) async {
    await _pump(
      tester,
      _view(
        space: const SpaceRow(
          space: 'ws_one',
          whoami: 'act_me',
          admin: true,
          members: [
            MemberRow(
              id: 'act_ada',
              nick: 'ada-nick',
              authoredName: 'Ada',
              admin: false,
            ),
          ],
          devices: [],
        ),
      ),
    );

    expect(find.text('Ada'), findsOneWidget);
    expect(find.text('ada-nick'), findsOneWidget);
    expect(find.text('act_ada'), findsOneWidget);
  });

  testWidgets('without a card, the nick is still just a nick', (tester) async {
    await _pump(
      tester,
      _view(
        space: const SpaceRow(
          space: 'ws_one',
          whoami: 'act_me',
          admin: true,
          members: [
            MemberRow(id: 'act_ada', nick: 'ada-nick', admin: false),
          ],
          devices: [],
        ),
      ),
    );

    expect(find.text('ada-nick'), findsOneWidget);
    expect(find.text('act_ada'), findsOneWidget);
    expect(find.text('Ada'), findsNothing);
  });
}
