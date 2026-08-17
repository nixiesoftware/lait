library;

import 'dart:convert';

import 'package:astrolabe/src/core/client.dart';
import 'package:astrolabe/src/shell/displays.dart';
import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/material.dart' show MaterialApp, Scaffold;
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

const _fingerprint =
    '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef';
const _certificatePem = '-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n';

ClientView _viewWithCustody(DisplayIdentifierCustodyRow? custody) => ClientView(
      loading: false,
      library_: const [],
      heads: const [],
      devices: const [],
      storage: const [],
      orbits: const [],
      notices: const [],
      failures: const [],
      inFlight: const [],
      display: DisplayFacts(
        instance: '0123456789abcdef0123456789abcdef',
        label: 'Astrolabe on studio-pc',
        origin: 'https://192.0.2.10:7443',
        certificateSha256: _fingerprint,
        certificatePem: _certificatePem,
        surfaces: const [],
        devices: const [],
        assignments: const [],
        pendingPairings: const [],
        identifierCustody: custody,
      ),
    );

ClientView _view() => const ClientView(
      loading: false,
      library_: [],
      heads: [],
      devices: [],
      storage: [],
      orbits: [],
      notices: [],
      failures: [],
      inFlight: [],
      display: DisplayFacts(
        instance: '0123456789abcdef0123456789abcdef',
        label: 'Astrolabe on studio-pc',
        origin: 'https://192.0.2.10:7443',
        certificateSha256: _fingerprint,
        certificatePem: _certificatePem,
        surfaces: [],
        devices: [],
        assignments: [],
        pendingPairings: [],
        identifierCustody: DisplayIdentifierCustodyRow(
          slots: ['recovery-key'],
          portable: true,
        ),
      ),
    );

void main() {
  testWidgets('copy setup emits the exact pinned receiver bootstrap',
      (tester) async {
    String? copied;
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(SystemChannels.platform, (call) async {
      if (call.method == 'Clipboard.setData') {
        copied = (call.arguments as Map<Object?, Object?>)['text'] as String?;
      }
      return null;
    });
    addTearDown(() {
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(SystemChannels.platform, null);
    });

    tester.view.physicalSize = const Size(900, 760);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      MaterialApp(
        theme: covalenceTheme(const ThemeConfig()),
        home: ClientScope(
          client: Client.canned(_view()),
          child: const Scaffold(body: DisplaysPage()),
        ),
      ),
    );

    await tester.tap(find.text('Copy setup'));
    await tester.pump();

    expect(
      jsonDecode(copied!),
      {
        'protocol_major': 1,
        'trust': {
          'kind': 'pinned_certificate',
          'origin': 'https://192.0.2.10:7443',
          'sha256': _fingerprint,
        },
        'certificate_pem': _certificatePem,
        'rendezvous': null,
      },
    );
  });

  testWidgets('a coordinator with no way off this machine says so',
      (tester) async {
    tester.view.physicalSize = const Size(900, 760);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      MaterialApp(
        theme: covalenceTheme(const ThemeConfig()),
        home: ClientScope(
          client: Client.canned(
            _viewWithCustody(const DisplayIdentifierCustodyRow(
              slots: ['windows-dpapi'],
              portable: false,
            )),
          ),
          child: const Scaffold(body: DisplaysPage()),
        ),
      ),
    );

    // The unlock paths are named, and the consequence is stated before the
    // machine is gone rather than after.
    expect(find.textContaining('this Windows profile'), findsOneWidget);
    expect(
      find.textContaining('Every unlock path is bound to this machine'),
      findsOneWidget,
    );
    expect(find.textContaining('need pairing again'), findsOneWidget);
  });

  testWidgets('a coordinator that never reported custody is not accused of '
      'having none', (tester) async {
    tester.view.physicalSize = const Size(900, 760);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      MaterialApp(
        theme: covalenceTheme(const ThemeConfig()),
        home: ClientScope(
          client: Client.canned(_viewWithCustody(null)),
          child: const Scaffold(body: DisplaysPage()),
        ),
      ),
    );

    // A daemon older than the custody split sends no such field. Unmeasured,
    // not zero — the warning belongs to a coordinator that answered.
    expect(find.textContaining('not reported'), findsOneWidget);
    expect(
      find.textContaining('Every unlock path is bound to this machine'),
      findsNothing,
    );
  });

  testWidgets('a second unlock path is offered once, and only where it is '
      'missing', (tester) async {
    tester.view.physicalSize = const Size(900, 760);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    await tester.pumpWidget(
      MaterialApp(
        theme: covalenceTheme(const ThemeConfig()),
        home: ClientScope(
          client: Client.canned(
            _viewWithCustody(const DisplayIdentifierCustodyRow(
              slots: ['recovery-key'],
              portable: true,
            )),
          ),
          child: const Scaffold(body: DisplaysPage()),
        ),
      ),
    );
    expect(find.text('Add a passphrase'), findsOneWidget);

    // Already held: the store refuses a second passphrase slot, and a control
    // that would be refused is one the surface should not draw.
    await tester.pumpWidget(
      MaterialApp(
        theme: covalenceTheme(const ThemeConfig()),
        home: ClientScope(
          client: Client.canned(
            _viewWithCustody(const DisplayIdentifierCustodyRow(
              slots: ['recovery-key', 'passphrase'],
              portable: true,
            )),
          ),
          child: const Scaffold(body: DisplaysPage()),
        ),
      ),
    );
    expect(find.text('Add a passphrase'), findsNothing);
  });

  testWidgets('a portable coordinator states the cost without the warning',
      (tester) async {
    tester.view.physicalSize = const Size(900, 760);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);
    await tester.pumpWidget(
      MaterialApp(
        theme: covalenceTheme(const ThemeConfig()),
        home: ClientScope(
          client: Client.canned(
            _viewWithCustody(const DisplayIdentifierCustodyRow(
              slots: ['recovery-key', 'passphrase'],
              portable: true,
            )),
          ),
          child: const Scaffold(body: DisplaysPage()),
        ),
      ),
    );

    expect(find.textContaining('this identity, a passphrase'), findsOneWidget);
    expect(
      find.textContaining('Every unlock path is bound to this machine'),
      findsNothing,
    );
  });
}
