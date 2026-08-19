library;

import 'package:astrolabe/src/core/client.dart';
import 'package:astrolabe/src/shell/record.dart';
import 'package:covalence/covalence.dart';
import 'package:flutter/material.dart' show Brightness, MaterialApp, Scaffold;
import 'package:flutter_test/flutter_test.dart';

ClientView _view({
  List<NoticeRow> notices = const [],
  List<FailureRow> failures = const [],
  List<String> inFlight = const [],
}) =>
    ClientView(
      loading: false,
      library_: const [],
      host: const HostFacts(
        version: '0.7.11',
        identityHome: r'C:\identity',
        spacesRoot: r'C:\spaces',
        orbitCount: 1,
      ),
      heads: const [
        HeadRow(
          id: 'identity:default',
          kind: 'browser',
          origin: 'http://127.0.0.1:55170/',
          owned: true,
  state: 'running',
        ),
      ],
      devices: const [],
      storage: const [],
      orbits: const [],
      notices: notices,
      failures: failures,
      inFlight: inFlight,
    );

Future<void> _pump(WidgetTester tester, ClientView view) => tester.pumpWidget(
      MaterialApp(
        theme: covalenceTheme(
          const ThemeConfig(brightness: Brightness.dark),
        ),
        home: ClientScope(
          client: Client.canned(view),
          child: const Scaffold(body: OperationalBar()),
        ),
      ),
    );

void main() {
  testWidgets(
      'launch records are deduplicated and credentials stay out of chrome',
      (tester) async {
    const launched =
        'http://127.0.0.1:55170/?ticket=single-use-secret#fragment';
    await _pump(
      tester,
      _view(
        notices: const [
          NoticeRow(said: 'opened World', launched: launched),
          NoticeRow(said: 'opened World', launched: launched),
        ],
      ),
    );

    expect(find.text('Opened http://127.0.0.1:55170/'), findsOneWidget);
    expect(find.textContaining('single-use-secret'), findsNothing);
    expect(find.text('Local identity online'), findsOneWidget);
  });

  testWidgets('an in-flight open becomes one concise footer activity',
      (tester) async {
    await _pump(tester, _view(inFlight: const ['open:orb_one/']));

    expect(find.text('Starting World…'), findsOneWidget);
  });
}
