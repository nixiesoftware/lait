/// Big Picture: what a member screen draws, and what it refuses to draw.
///
/// These press the real surface against a canned view. The interesting cases
/// are all absences with a kind: an item this screen cannot draw, a source that
/// went partial, and a refresh that did not answer. Each has to read
/// differently from the others and from "fine".
library;

import 'dart:typed_data';

import 'package:astrolabe/src/core/client.dart';
import 'package:astrolabe/src/shell/present.dart';
import 'package:astrolabe/src/shell/window.dart';
import 'package:covalence/covalence.dart' hide Image, Surface;
import 'package:flutter/material.dart' show MaterialApp, Scaffold;
import 'package:flutter/services.dart' show LogicalKeyboardKey;
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

/// A one-pixel PNG. Real bytes, because `Image.memory` decodes them and a
/// placeholder would make the frame case pass without drawing anything.
final _png = Uint8List.fromList(const [
  0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, //
  0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
  0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
  0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
  0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41,
  0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
  0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
  0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
  0x42, 0x60, 0x82,
]);

PresentedItem _item(PresentedScene scene) => PresentedItem(
      id: 'one',
      durationMs: 60000,
      assessment: 'current',
      scene: scene,
    );

PresentationFacts _presenting({
  required List<PresentedItem> items,
  String assessment = 'current',
  List<String> partialReasons = const [],
  String? failure,
  bool rendered = true,
  bool chosen = true,
}) =>
    PresentationFacts(
      chosen: chosen
          ? const PresentationChoice(
              orbit: 'ws_one',
              world: 'com.lait.signage',
              surface: 'signage.program',
              title: 'Lobby loop',
            )
          : null,
      failure: failure,
      program: rendered
          ? PresentedProgram(
              assessment: assessment,
              partialReasons: partialReasons,
              cycle: 'hold_last',
              items: items,
            )
          : null,
    );

/// Records what the surface asserted about the display.
class _Chrome implements WindowControlHost {
  bool? fullScreen;
  int assertions = 0;

  @override
  Future<void> setFullScreen(bool full) async {
    fullScreen = full;
    assertions++;
  }

  @override
  Future<void> configureOwned(OwnedWindowConfiguration configuration) async {}
  @override
  Future<void> close() async {}
  @override
  Future<void> hide() async {}
  @override
  Future<bool> isMaximized() async => false;
  @override
  Future<void> minimize() async {}
  @override
  Future<void> startDragging() async {}
  @override
  Future<void> toggleMaximize() async {}
}

Widget _under(PresentationFacts presenting, {void Function(ActionRequest)? on}) =>
    MaterialApp(
      theme: covalenceTheme(const ThemeConfig()),
      home: ClientScope(
        client: Client.canned(
          ClientView(
            loading: false,
            library_: const [],
            heads: const [],
            devices: const [],
            storage: const [],
            orbits: const [],
            notices: const [],
            failures: const [],
            inFlight: const [],
            presentation: presenting,
          ),
          onDispatch: on ?? (_) {},
        ),
        child: Scaffold(body: BigPictureSurface(presentation: presenting)),
      ),
    );

void main() {
  testWidgets('a frame is drawn, and the way out is on screen', (tester) async {
    await tester.pumpWidget(_under(_presenting(items: [
      _item(PresentedScene.frame(
        mediaType: 'png',
        width: 1,
        height: 1,
        bytes: _png,
      )),
    ])));
    await tester.pump();

    expect(find.byType(Image), findsOneWidget);
    expect(find.text('Lobby loop'), findsOneWidget);
    // Leaving is always available and says how — a fullscreen surface with no
    // stated exit is a trap, not a screen.
    expect(find.text('Esc to leave'), findsOneWidget);
  });

  testWidgets('an item this screen cannot draw refuses visibly', (tester) async {
    await tester.pumpWidget(_under(_presenting(items: [
      _item(const PresentedScene.unsupported(output: 'media')),
    ])));
    await tester.pump();

    // Not dropped, and not blanked: dropping would leave a program nobody
    // authored, and blanking would blame the source for this screen's limit.
    expect(find.textContaining('cannot draw media'), findsOneWidget);
    expect(find.byType(Image), findsNothing);
  });

  testWidgets('a partial source names its reasons over the frame',
      (tester) async {
    await tester.pumpWidget(_under(_presenting(
      items: [
        _item(PresentedScene.frame(
          mediaType: 'png',
          width: 1,
          height: 1,
          bytes: _png,
        )),
      ],
      assessment: 'partial',
      partialReasons: const ['degraded_source'],
    )));
    await tester.pump();

    expect(find.byType(Image), findsOneWidget);
    expect(find.textContaining('partial'), findsOneWidget);
    expect(find.textContaining('degraded_source'), findsOneWidget);
  });

  testWidgets('a failed refresh is stated beside the frame, not instead of it',
      (tester) async {
    await tester.pumpWidget(_under(_presenting(
      items: [
        _item(PresentedScene.frame(
          mediaType: 'png',
          width: 1,
          height: 1,
          bytes: _png,
        )),
      ],
      failure: 'the daemon did not answer',
    )));
    await tester.pump();

    // Delivery and source truth are separate states. The screen keeps showing
    // what it last verified and says the re-ask failed.
    expect(find.byType(Image), findsOneWidget);
    expect(find.textContaining('Could not refresh'), findsOneWidget);
  });

  testWidgets('an empty program reads as empty, not as nothing rendered',
      (tester) async {
    await tester.pumpWidget(_under(_presenting(items: const [])));
    await tester.pump();

    expect(find.text('This program has no items'), findsOneWidget);
  });

  testWidgets('a screen that has never rendered says so differently',
      (tester) async {
    await tester.pumpWidget(_under(
      _presenting(items: const [], rendered: false),
    ));
    await tester.pump();

    expect(find.text('Nothing to show'), findsOneWidget);
  });

  testWidgets('presenting takes the display and giving up returns it',
      (tester) async {
    final chrome = _Chrome();
    final presenting = _presenting(items: [
      _item(PresentedScene.frame(
        mediaType: 'png',
        width: 1,
        height: 1,
        bytes: _png,
      )),
    ]);
    await tester.pumpWidget(
      MaterialApp(
        theme: covalenceTheme(const ThemeConfig()),
        home: ClientScope(
          client: Client.canned(
            ClientView(
              loading: false,
              library_: const [],
              heads: const [],
              devices: const [],
              storage: const [],
              orbits: const [],
              notices: const [],
              failures: const [],
              inFlight: const [],
              presentation: presenting,
            ),
          ),
          child: Scaffold(
            body: BigPictureSurface(
              presentation: presenting,
              chrome: chrome,
            ),
          ),
        ),
      ),
    );
    await tester.pump();

    // Fullscreen, not maximised: a maximised window keeps its frame and leaves
    // the taskbar over it, which is a large window rather than a screen.
    expect(chrome.fullScreen, isTrue);

    // Leaving by any route gives the display back. A client that exited Big
    // Picture and kept the screen would be a window nobody could get behind.
    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump();
    expect(chrome.fullScreen, isFalse);
    expect(chrome.assertions, 2);
  });

  testWidgets('a screen with nothing chosen chooses in place', (tester) async {
    tester.view.physicalSize = const Size(1200, 900);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    final presenting =
        _presenting(items: const [], rendered: false, chosen: false);
    await tester.pumpWidget(
      MaterialApp(
        theme: covalenceTheme(const ThemeConfig()),
        home: ClientScope(
          client: Client.canned(ClientView(
            loading: false,
            library_: const [],
            heads: const [],
            devices: const [],
            storage: const [],
            orbits: const [
              OrbitRow(space: 'ws_one', name: 'Blueprint', path: '/tmp/one'),
            ],
            notices: const [],
            failures: const [],
            inFlight: const [],
            display: const DisplayFacts(
              instance: '0123456789abcdef0123456789abcdef',
              label: 'Astrolabe on studio-pc',
              origin: 'https://192.0.2.10:7443',
              certificateSha256: 'aa',
              certificatePem: 'pem',
              surfaces: [
                DisplaySurfaceRow(
                  world: 'com.lait.signage',
                  surface: 'signage.program',
                  title: 'Signage program',
                  contractVersion: 1,
                  outputs: ['frame'],
                ),
              ],
              devices: [],
              assignments: [],
              pendingPairings: [],
              identifierCustody: null,
            ),
            presentation: presenting,
          )),
          child: Scaffold(body: BigPictureSurface(presentation: presenting)),
        ),
      ),
    );
    await tester.pump();

    // Choosing happens on the screen the person just made, not in a dialog in
    // front of the control that makes it. The press was the consent.
    expect(find.text('What should this screen show?'), findsOneWidget);
    expect(find.text('Esc to leave'), findsOneWidget);
  });

  testWidgets('a screen that cannot draw anything says which reason',
      (tester) async {
    tester.view.physicalSize = const Size(1200, 900);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.resetPhysicalSize);
    addTearDown(tester.view.resetDevicePixelRatio);

    await tester.pumpWidget(_under(
      _presenting(items: const [], rendered: false, chosen: false),
    ));
    await tester.pump();

    // Stated on the screen at ten feet rather than in a tooltip on a disabled
    // control nobody is standing close enough to read — and naming *which*
    // absence, since a coordinator that has not answered and a build with no
    // surfaces send you looking in different places.
    expect(find.text('Nothing to show here'), findsOneWidget);
    expect(
      find.textContaining('display coordinator has not answered'),
      findsOneWidget,
    );
  });

  testWidgets('escape asks to leave', (tester) async {
    final asked = <ActionRequest>[];
    await tester.pumpWidget(_under(
      _presenting(items: [
        _item(PresentedScene.frame(
          mediaType: 'png',
          width: 1,
          height: 1,
          bytes: _png,
        )),
      ]),
      on: asked.add,
    ));
    await tester.pump();

    await tester.sendKeyEvent(LogicalKeyboardKey.escape);
    await tester.pump();

    expect(asked, [const ActionRequest.leavePresentation()]);
  });
}
