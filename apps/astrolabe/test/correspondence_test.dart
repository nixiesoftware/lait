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
}) =>
    CorrespondenceFacts(
      myDevice: null,
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
