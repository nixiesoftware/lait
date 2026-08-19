/// The chat window body, driven against canned views.
///
/// No bridge, no core, no daemon, no window: a canned [ClientView] and a
/// recorder for what each control asks. The rules under test are the chat's —
/// a message drawn on the side of whoever sent it, an invitation as its own
/// widget, a provenance disagreement shown rather than resolved, and the
/// composer's send.
library;

import 'package:astrolabe/src/core/client.dart';
import 'package:astrolabe/src/shell/correspondence.dart';
import 'package:astrolabe/src/shell/theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

ClientView _view({
  CorrespondenceFacts? correspondence,
  List<String> inFlight = const [],
}) =>
    ClientView(
      loading: false,
      library_: const [],
      host: null,
      heads: const [],
      devices: const [],
      storage: const [],
      orbits: const [],
      notices: const [],
      failures: const [],
      correspondence: correspondence,
      inFlight: inFlight,
    );

int _nowSecs() => DateTime.now().millisecondsSinceEpoch ~/ 1000;

ChatMessageRow _message({
  bool mine = false,
  String kind = 'message',
  String? body = 'hello',
  int? sentAt,
  String fromDevice = 'device:ada-laptop',
  bool provenanceAgrees = true,
}) =>
    ChatMessageRow(
      mine: mine,
      kind: kind,
      body: body,
      sentAt: BigInt.from(sentAt ?? _nowSecs()),
      fromDevice: fromDevice,
      provenanceAgrees: provenanceAgrees,
    );

CorrespondenceFacts _facts({
  List<ChatMessageRow> messages = const [],
  List<String> openTabs = const ['ada'],
  String? activeTab = 'ada',
  String? myReach,
}) =>
    CorrespondenceFacts(
      myDevice: null,
      myReach: myReach,
      me: null,
      contacts: [
        ContactRow(
          id: 'ada',
          name: 'Ada',
          devices: const ['device:ada-laptop'],
          added: true,
          isAgent: false,
          parentId: null,
          parentName: null,
          unread: 0,
        ),
      ],
      conversations: [
        ConversationRow(peerId: 'ada', peerName: 'Ada', messages: messages),
      ],
      openTabs: openTabs,
      activeTab: activeTab,
    );

/// What the hosted backend actually emits when nobody has been reached yet:
/// this identity's own conversation, open and active. Never an empty tab list —
/// `PostBackend::snapshot` cannot produce one, which is why gating the exchange
/// on "no tabs" hid it on the only arm where it works.
CorrespondenceFacts _postShaped({String? myReach}) => CorrespondenceFacts(
      myDevice: 'device:mine',
      myReach: myReach,
      me: 'prf_me',
      contacts: [
        ContactRow(
          id: 'prf_me',
          name: 'You (over the Post)',
          devices: const ['device:mine'],
          added: true,
          isAgent: false,
          parentId: null,
          parentName: null,
          unread: 0,
        ),
      ],
      conversations: const [
        ConversationRow(peerId: 'prf_me', peerName: 'You (over the Post)', messages: []),
      ],
      openTabs: const ['prf_me'],
      activeTab: 'prf_me',
    );

Future<List<ActionRequest>> _pump(
  WidgetTester tester,
  ClientView view,
) async {
  tester.view.physicalSize = const Size(1400, 1000);
  tester.view.devicePixelRatio = 1;
  await tester.binding.setSurfaceSize(const Size(1400, 1000));
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  addTearDown(() => tester.binding.setSurfaceSize(null));

  final asked = <ActionRequest>[];
  await tester.pumpWidget(
    MaterialApp(
      theme: astrolabeTheme(Brightness.light),
      home: ClientScope(
        client: Client.canned(view, onDispatch: asked.add),
        child: const Scaffold(body: ChatBody()),
      ),
    ),
  );
  await tester.pump(const Duration(milliseconds: 200));
  return asked;
}

void main() {
  testWidgets('with no open tabs it invites you to start from the book',
      (tester) async {
    await _pump(
      tester,
      _view(correspondence: _facts(openTabs: const [], activeTab: null)),
    );
    expect(
      find.text('Open a conversation from the address book.'),
      findsOneWidget,
    );
  });

  testWidgets('the conversation draws messages, mine and theirs',
      (tester) async {
    await _pump(
      tester,
      _view(
        correspondence: _facts(messages: [
          _message(mine: false, body: 'saw your issue'),
          _message(mine: true, body: 'on it'),
        ]),
      ),
    );
    expect(find.text('saw your issue'), findsOneWidget);
    expect(find.text('on it'), findsOneWidget);
    // A recent message earns a Today separator.
    expect(find.text('Today'), findsOneWidget);
  });

  testWidgets('typing and the send arrow ask to send to the active peer',
      (tester) async {
    final asked = await _pump(tester, _view(correspondence: _facts()));
    await tester.enterText(find.byKey(const ValueKey('chat-input')), 'hi Ada');
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('chat-send')));
    await tester.pump();
    expect(asked, [const ActionRequest.sendMessage(to: 'ada', body: 'hi Ada')]);
  });

  testWidgets('a received message with disagreeing provenance says so',
      (tester) async {
    await _pump(
      tester,
      _view(
        correspondence: _facts(messages: [
          _message(mine: false, body: 'hi', provenanceAgrees: false),
        ]),
      ),
    );
    expect(find.text('delivered by a different device'), findsOneWidget);
  });

  testWidgets('an invitation draws its own widget, not a text bubble',
      (tester) async {
    await _pump(
      tester,
      _view(
        correspondence: _facts(messages: [
          _message(mine: false, kind: 'invitation', body: null),
        ]),
      ),
    );
    expect(find.text('Invitation to a Space'), findsOneWidget);
    expect(find.text('Open'), findsOneWidget);
  });

  testWidgets(
      'an unbuilt control is drawn disabled, and pressing it asks for nothing',
      (tester) async {
    // The three controls on this surface with nothing behind them yet: the
    // invitation's Open and Decline (no action carries entering a Space), and
    // the composer's attach (`Content` has no attachment variant).
    //
    // Each was previously wired to an empty closure, which draws a control that
    // looks live, accepts the press, and does nothing — the failure this suite
    // could not see, because a test that asserts a button *renders* passes
    // either way. Pressing them is the assertion that matters.
    final asked = await _pump(
      tester,
      _view(
        correspondence: _facts(messages: [
          _message(mine: false, kind: 'invitation', body: null),
        ]),
      ),
    );

    await tester.tap(find.text('Open'), warnIfMissed: false);
    await tester.tap(find.text('Decline'), warnIfMissed: false);
    await tester.tap(
      find.bySemanticsLabel(RegExp('^Attach a file')),
      warnIfMissed: false,
    );
    await tester.pump();

    expect(
      asked,
      isEmpty,
      reason: 'a control with nothing behind it asks for nothing when pressed',
    );
    // And it says so rather than looking ready: the label carries the reason,
    // so the disabled state is legible without hovering for a tooltip.
    expect(find.bySemanticsLabel(RegExp('not available yet')), findsWidgets);
  });

  testWidgets('with nobody reached yet, the exchange is what the chat offers',
      (tester) async {
    final asked = await _pump(tester, _view(correspondence: _postShaped()));

    await tester.tap(find.byKey(const ValueKey('reach-share')));
    await tester.pump();
    expect(asked, [const ActionRequest.shareReach()]);

    asked.clear();
    // An empty paste asks for nothing — a refusal round trip is not how a
    // surface says "you have not typed anything yet".
    await tester.tap(find.byKey(const ValueKey('reach-add')));
    await tester.pump();
    expect(asked, isEmpty);

    await tester.enterText(find.byKey(const ValueKey('reach-paste')), '  abc  ');
    await tester.tap(find.byKey(const ValueKey('reach-add')));
    await tester.pump();
    expect(asked, [const ActionRequest.addCorrespondent(announcement: 'abc')]);
  });

  testWidgets('a published card is shown to copy, not re-published',
      (tester) async {
    // Announcing bumps the epoch and appends to the kinship log, so a surface
    // that re-asked on every build would grow the log for nothing.
    final asked = await _pump(
      tester,
      _view(correspondence: _postShaped(myReach: 'aebagbafaydqqcik')),
    );
    expect(find.byKey(const ValueKey('reach-card')), findsOneWidget);
    expect(find.byKey(const ValueKey('reach-share')), findsNothing);
    expect(asked, isEmpty);
  });

  testWidgets('sharing already under way does not ask twice', (tester) async {
    final asked = await _pump(
      tester,
      _view(
        correspondence: _postShaped(),
        inFlight: [ActionKeys.shareReach],
      ),
    );
    await tester.tap(
      find.byKey(const ValueKey('reach-share')),
      warnIfMissed: false,
    );
    await tester.pump();
    expect(asked, isEmpty);
  });

  testWidgets('once someone is reached the transcript returns, and the exchange '
      'is a control rather than the body', (tester) async {
    // The regression guard for the defect this fixture exists to expose: the
    // offer must not be gated on a tab state a backend never emits, and must
    // not squat on the conversation once there is one.
    await _pump(tester, _view(correspondence: _facts()));
    expect(find.byKey(const ValueKey('reach-paste')), findsNothing);
    expect(find.byKey(const ValueKey('chat-input')), findsOneWidget);

    await tester.tap(find.byKey(const ValueKey('reach-toggle')));
    await tester.pump();
    expect(find.byKey(const ValueKey('reach-paste')), findsOneWidget);
    expect(find.byKey(const ValueKey('chat-input')), findsNothing);

    await tester.tap(find.byKey(const ValueKey('reach-toggle')));
    await tester.pump();
    expect(find.byKey(const ValueKey('chat-input')), findsOneWidget);
  });

  testWidgets('Block asks to block the active person', (tester) async {
    final asked = await _pump(
      tester,
      _view(correspondence: _facts(messages: [_message()])),
    );
    await tester.tap(find.text('Block'));
    await tester.pump();
    expect(asked, [const ActionRequest.blockSender(person: 'ada')]);
  });
}
