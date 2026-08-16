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
}
