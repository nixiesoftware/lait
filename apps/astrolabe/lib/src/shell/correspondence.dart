/// The chat window.
///
/// Correspondence is a conversation, never an inbox. A person is reached from
/// the address book; a click opens a chat here. The window's chrome IS its
/// tabs — browser-style, in the caption band beside the system controls, each
/// with the correspondent's face as its favicon — so it bends no expectation a
/// tabbed OS window does not already set.
///
/// It draws only what the shared model holds. Which tabs are open, which is
/// focused, and every message are [ClientView.correspondence] — the same view
/// the address book reads, so a click there opens the tab this draws. The one
/// draft it owns is the line being typed.
library;

import 'package:covalence/covalence.dart' hide Image, Surface;
// `SelectableText` because the card is long and its whole purpose is being
// copied out; a plain `Text` would make a person retype it.
import 'package:flutter/material.dart'
    show MaterialApp, Scaffold, SelectableText, ThemeMode;
import 'package:flutter/services.dart' show LogicalKeyboardKey;
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import 'face.dart';
import 'host.dart';
import 'theme.dart';
import 'type.dart';
import 'window.dart';

const Size _chatOpening = Size(760, 660);
const Size _chatNarrowest = Size(520, 480);

/// A run of same-sender messages is broken when the sender flips, the day
/// turns, or this much quiet passes — the gap that earns a divider.
const Duration _longGap = Duration(hours: 1);

/// The whole window: chrome that is the tabs, and the conversation inside.
class CorrespondenceApp extends StatelessWidget {
  const CorrespondenceApp({super.key, required this.client});

  final Client client;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Chat',
      debugShowCheckedModeBanner: false,
      theme: astrolabeTheme(Brightness.light),
      darkTheme: astrolabeTheme(Brightness.dark),
      themeMode: ThemeMode.dark,
      home: ClientScope(
        client: client,
        child: Scaffold(
          body: AstrolabeWindowFrame.secondary(
            title: 'Chat',
            nativeTitle: 'Chat — Astrolabe',
            nativeKey: correspondenceWindowKey,
            size: _chatOpening,
            minimumSize: _chatNarrowest,
            dark: true,
            captionBuilder: (context, _) => const _ChatTabs(),
            body: const ChatBody(),
          ),
        ),
      ),
    );
  }
}

/// The browser-style tab strip, in the caption band. One tab per open
/// conversation: the correspondent's face as favicon, their name, a close.
class _ChatTabs extends StatelessWidget {
  const _ChatTabs();

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    final facts = view.correspondence;
    final tabs = facts?.openTabs ?? const <String>[];
    if (tabs.isEmpty) {
      return Align(
        alignment: Alignment.centerLeft,
        child: Text('Chat', style: context.bodyStyle),
      );
    }
    final active = facts?.activeTab ?? tabs.first;
    return SingleChildScrollView(
      scrollDirection: Axis.horizontal,
      child: Row(
        children: [
          // Each tab gates on its own keys. The core has emitted these all
          // along and nothing consulted them, so a tab stayed live across the
          // round trip — long enough to press it four times and read three
          // answers, which is the thing `in_flight` exists to prevent.
          for (final id in tabs)
            _Tab(
              name: _nameOf(facts, id),
              agent: _isAgent(facts, id),
              active: id == active,
              onTap: view.inFlight.contains(ActionKeys.focusConversation(id))
                  ? null
                  : () => client
                      .dispatch(ActionRequest.focusConversation(person: id)),
              onClose: view.inFlight.contains(ActionKeys.closeConversation(id))
                  ? null
                  : () => client
                      .dispatch(ActionRequest.closeConversation(person: id)),
            ),
          t.gap.x(Space.sm),
        ],
      ),
    );
  }
}

class _Tab extends StatelessWidget {
  const _Tab({
    required this.name,
    required this.agent,
    required this.active,
    required this.onTap,
    required this.onClose,
  });

  final String name;
  final bool agent;
  final bool active;

  /// Null while this tab's own focus is in flight. `GestureDetector` takes a
  /// null callback as "not interactive", so the gate needs no second flag.
  final VoidCallback? onTap;

  /// Null while this tab's own close is in flight.
  final VoidCallback? onClose;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return GestureDetector(
      onTap: onTap,
      behavior: HitTestBehavior.opaque,
      child: Container(
        constraints: const BoxConstraints(maxWidth: 180),
        padding: t.padding.symmetric(h: Space.sm),
        decoration: BoxDecoration(
          color: active ? context.surface.l100 : null,
          border: Border(
            bottom: BorderSide(
              color: active ? t.brand.l800 : const Color(0x00000000),
              width: t.stroke.sm,
            ),
          ),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            FacePlate(picture: null, name: name, size: 18),
            t.gap.x(Space.xs),
            Flexible(
              child: Text(
                name,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: active
                    ? context.labelStyle
                        .copyWith(color: context.text.l950)
                    : context.labelStyle,
              ),
            ),
            if (agent) ...[
              t.gap.x(Space.xxs),
              const Badge(label: 'AI', variant: BadgeVariant.outline),
            ],
            t.gap.x(Space.xs),
            GestureDetector(
              onTap: onClose,
              behavior: HitTestBehavior.opaque,
              child: Icon(
                AppIcons.close,
                size: t.font.sm,
                color: context.text.l800,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// The focused conversation, or an invitation to open one.
class ChatBody extends StatefulWidget {
  const ChatBody({super.key});

  @override
  State<ChatBody> createState() => _ChatBodyState();
}

class _ChatBodyState extends State<ChatBody> {
  final Map<String, TextEditingController> _drafts = {};

  TextEditingController _draft(String peer) =>
      _drafts.putIfAbsent(peer, TextEditingController.new);

  @override
  void dispose() {
    for (final controller in _drafts.values) {
      controller.dispose();
    }
    super.dispose();
  }

  void _send(Client client, String peer) {
    final draft = _draft(peer);
    final body = draft.text.trim();
    if (body.isEmpty) return;
    client.dispatch(ActionRequest.sendMessage(to: peer, body: body));
    draft.clear();
    setState(() {});
  }

  /// Whether the person asked to reach somebody new. Draft state, which is the
  /// one kind of state a surface may hold.
  bool _reaching = false;

  @override
  Widget build(BuildContext context) {
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    final facts = view.correspondence;
    final tabs = facts?.openTabs ?? const <String>[];
    if (tabs.isEmpty) return const _EmptyChat();

    final active = facts?.activeTab ?? tabs.first;
    final conversation = facts?.conversations
        .where((conversation) => conversation.peerId == active)
        .firstOrNull;
    if (conversation == null) return const SizedBox();

    // Reaching somebody is an act, not an empty state. A backend always has at
    // least this identity's own conversation open, so gating the offer on "no
    // tabs" put it behind a condition the hosted plane can never reach. Offered
    // unprompted while there is nobody to talk to, and from the header after.
    final me = facts?.me;
    final alone = me != null &&
        (facts?.contacts ?? const <ContactRow>[])
            .every((contact) => contact.id == me);

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _ConversationHeader(
          name: conversation.peerName,
          agent: _isAgent(facts, active),
          collecting: view.inFlight.contains(ActionKeys.collectMail),
          blocking: view.inFlight.contains(ActionKeys.blockSender(active)),
          reaching: _reaching || alone,
          onReach: alone ? null : () => setState(() => _reaching = !_reaching),
          onCollect: () => client.dispatch(const ActionRequest.collectMail()),
          onBlock: () =>
              client.dispatch(ActionRequest.blockSender(person: active)),
        ),
        if (_reaching || alone)
          const Expanded(child: _Reach())
        else ...[
          Expanded(child: _Transcript(conversation: conversation)),
          _Composer(
            draft: _draft(active),
            sending: view.inFlight.contains(ActionKeys.sendMessage(active)),
            onSend: () => _send(client, active),
          ),
        ],
      ],
    );
  }
}

/// No conversation at all — the Demo fixture and an unconnected plane. A hosted
/// plane always has this identity's own conversation, so it never lands here.
class _EmptyChat extends StatelessWidget {
  const _EmptyChat();

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Padding(
        padding: context.tokens.padding.all(Space.xl3),
        child: Text(
          'Open a conversation from the address book.',
          textAlign: TextAlign.center,
          style: context.proseStyle,
        ),
      ),
    );
  }
}

/// Reaching somebody for the first time: what you hand over, and a field for
/// what they hand back. One exchange, over whatever channel two people already
/// share.
///
/// What is swapped is an `Announcement` — not a Card (the address book's, and
/// evidence of nothing) and not an address (the directory's, and short).
class _Reach extends StatelessWidget {
  const _Reach();

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    final card = view.correspondence?.myReach;
    final sharing = view.inFlight.contains(ActionKeys.shareReach);
    final adding = view.inFlight.contains(ActionKeys.addCorrespondent);

    return SingleChildScrollView(
      child: Padding(
        padding: t.padding.all(Space.xl3),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text('Reach someone new', style: context.labelStyle),
            t.gap.y(Space.xs),
            Text(
              'Swap these once, over any channel you already share. '
              'Yours names your devices; theirs names theirs.',
              style: context.proseStyle,
            ),
            t.gap.y(Space.md),
            if (card == null)
              Button(
                key: const ValueKey('reach-share'),
                onPressed: sharing
                    ? null
                    : () =>
                        client.dispatch(const ActionRequest.shareReach()),
                label: 'Show how to reach me',
                variant: ButtonVariant.secondary,
              )
            else
              SelectableText(
                card,
                key: const ValueKey('reach-card'),
                style: context.proseStyle.copyWith(fontFamily: 'monospace'),
                maxLines: 4,
              ),
            t.gap.y(Space.md),
            _AddCorrespondent(adding: adding),
          ],
        ),
      ),
    );
  }
}

/// Their announcement, pasted. Verified rather than trusted: `absorb`
/// re-derives the profile from its genesis and refuses a substituted device
/// set, so a forged one is caught here rather than believed.
class _AddCorrespondent extends StatefulWidget {
  const _AddCorrespondent({required this.adding});

  final bool adding;

  @override
  State<_AddCorrespondent> createState() => _AddCorrespondentState();
}

class _AddCorrespondentState extends State<_AddCorrespondent> {
  final _draft = TextEditingController();

  @override
  void dispose() {
    _draft.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final client = ClientScope.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Textarea(
          key: const ValueKey('reach-paste'),
          controller: _draft,
          hint: 'Paste theirs',
          minLines: 2,
          maxLines: 4,
        ),
        t.gap.y(Space.xs),
        Button(
          key: const ValueKey('reach-add'),
          onPressed: widget.adding
              ? null
              : () {
                  final pasted = _draft.text.trim();
                  if (pasted.isEmpty) {
                    return;
                  }
                  client.dispatch(
                    ActionRequest.addCorrespondent(announcement: pasted),
                  );
                  _draft.clear();
                },
          label: 'Add them',
          variant: ButtonVariant.primary,
        ),
      ],
    );
  }
}

/// A slim header: who this is, and the two acts on a whole conversation.
class _ConversationHeader extends StatelessWidget {
  const _ConversationHeader({
    required this.name,
    required this.agent,
    required this.collecting,
    required this.blocking,
    required this.reaching,
    required this.onReach,
    required this.onCollect,
    required this.onBlock,
  });

  final String name;
  final bool agent;
  final bool collecting;
  final bool blocking;

  /// Whether the exchange is showing instead of the transcript.
  final bool reaching;

  /// Null when there is nobody to talk to yet — the exchange is already the
  /// body, and a control that toggles back to an empty transcript is a control
  /// that does nothing worth doing.
  final VoidCallback? onReach;
  final VoidCallback onCollect;
  final VoidCallback onBlock;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Container(
      padding: t.padding.symmetric(h: Space.lg, v: Space.sm),
      decoration: BoxDecoration(
        border: Border(bottom: BorderSide(color: t.border.l500)),
      ),
      child: Row(
        children: [
          FacePlate(picture: null, name: name, size: 28),
          t.gap.x(Space.sm),
          Text(name, style: context.headingStyle),
          if (agent) ...[
            t.gap.x(Space.xs),
            const Badge(label: 'AI', variant: BadgeVariant.outline),
          ],
          const Spacer(),
          Button(
            key: const ValueKey('reach-toggle'),
            onPressed: onReach,
            icon: AppIcons.personAdd,
            semanticLabel: reaching ? 'Back to the conversation' : 'Reach someone new',
            variant: reaching ? ButtonVariant.secondary : ButtonVariant.ghost,
            size: ButtonSize.iconSm,
            tooltip: reaching ? 'Back to the conversation' : 'Reach someone new',
          ),
          t.gap.x(Space.xxs),
          Button(
            onPressed: collecting ? null : onCollect,
            icon: AppIcons.refresh,
            semanticLabel: 'Check for messages',
            variant: ButtonVariant.ghost,
            size: ButtonSize.iconSm,
            tooltip: 'Check for messages',
          ),
          t.gap.x(Space.xxs),
          Button(
            onPressed: blocking ? null : onBlock,
            label: 'Block',
            variant: ButtonVariant.destructiveGhost,
            size: ButtonSize.sm,
          ),
        ],
      ),
    );
  }
}

/// The messages, grouped by sender and parted by day and long quiet — the
/// standard shape of a chat transcript.
class _Transcript extends StatelessWidget {
  const _Transcript({required this.conversation});

  final ConversationRow conversation;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final messages = conversation.messages;
    if (messages.isEmpty) {
      return Center(child: Text('No messages yet.', style: context.proseStyle));
    }

    final items = <Widget>[];
    DateTime? prev;
    for (var i = 0; i < messages.length; i++) {
      final message = messages[i];
      final at = _at(message);
      final next = i + 1 < messages.length ? messages[i + 1] : null;
      final nextAt = next == null ? null : _at(next);

      final newDay = prev == null || !_sameDay(prev, at);
      final longGap = prev != null && !newDay && at.difference(prev) > _longGap;

      if (newDay) {
        items.add(_DateSeparator(label: _dayLabel(at)));
      } else if (longGap) {
        items.add(const _GapDivider());
      }

      final groupStarts =
          newDay || longGap || i == 0 || messages[i - 1].mine != message.mine;
      final groupEnds = next == null ||
          next.mine != message.mine ||
          !_sameDay(at, nextAt!) ||
          nextAt.difference(at) > _longGap;

      items.add(
        _MessageRow(
          message: message,
          peerName: conversation.peerName,
          groupStarts: groupStarts,
          groupEnds: groupEnds,
        ),
      );
      prev = at;
    }

    return ListView(
      padding: t.padding.all(Space.lg),
      children: items,
    );
  }
}

/// A centered date pill: Today, Yesterday, or a written date.
class _DateSeparator extends StatelessWidget {
  const _DateSeparator({required this.label});

  final String label;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Padding(
      padding: t.padding.symmetric(v: Space.md),
      child: Center(
        child: Container(
          padding: t.padding.symmetric(h: Space.sm, v: Space.xxs),
          decoration: BoxDecoration(
            color: context.surface.l100,
            borderRadius: t.radius.all(Space.md),
          ),
          child: Text(label, style: context.labelStyle),
        ),
      ),
    );
  }
}

/// A hairline for a long quiet within a day.
class _GapDivider extends StatelessWidget {
  const _GapDivider();

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Padding(
      padding: t.padding.symmetric(v: Space.md),
      child: Container(height: t.stroke.xxs, color: context.border.l500),
    );
  }
}

/// One message: on the side of whoever sent it, its component chosen by kind,
/// with the sender's name+time above the first of a group and their face below
/// the last.
class _MessageRow extends StatelessWidget {
  const _MessageRow({
    required this.message,
    required this.peerName,
    required this.groupStarts,
    required this.groupEnds,
  });

  final ChatMessageRow message;
  final String peerName;
  final bool groupStarts;
  final bool groupEnds;

  static const double _gutter = 34;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final mine = message.mine;
    final bubble = _MessageComponent(message: message);

    if (mine) {
      // Sent: right, no avatar, a small time under the last of the group.
      return Padding(
        padding: groupEnds
            ? t.padding.only(bottom: Space.md)
            : const EdgeInsets.only(bottom: 2),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.end,
          children: [
            bubble,
            if (groupEnds) ...[
              t.gap.y(Space.xxs),
              Text(_timeLabel(_at(message)), style: context.factLabelStyle),
            ],
          ],
        ),
      );
    }

    // Received: a left gutter that carries the face only on the last of the
    // group, the name+time above the first.
    return Padding(
      padding: groupEnds
            ? t.padding.only(bottom: Space.md)
            : const EdgeInsets.only(bottom: 2),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.end,
        children: [
          SizedBox(
            width: _gutter,
            child: groupEnds
                ? FacePlate(picture: null, name: peerName, size: 28)
                : null,
          ),
          t.gap.x(Space.xs),
          Flexible(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                if (groupStarts) ...[
                  Row(
                    children: [
                      Text(
                        peerName,
                        style: context.labelStyle
                            .copyWith(fontWeight: FontWeight.w600),
                      ),
                      t.gap.x(Space.xs),
                      Text(
                        _timeLabel(_at(message)),
                        style: context.factLabelStyle,
                      ),
                    ],
                  ),
                  t.gap.y(Space.xxs),
                ],
                bubble,
              ],
            ),
          ),
        ],
      ),
    );
  }
}

/// The seam where a message kind chooses its component. A new kind adds a case
/// here and its own widget; nothing else in the chat changes.
class _MessageComponent extends StatelessWidget {
  const _MessageComponent({required this.message});

  final ChatMessageRow message;

  @override
  Widget build(BuildContext context) {
    switch (message.kind) {
      case 'invitation':
        return _InvitationCard(message: message);
      case 'message':
      default:
        return _TextBubble(message: message);
    }
  }
}

class _TextBubble extends StatelessWidget {
  const _TextBubble({required this.message});

  final ChatMessageRow message;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final mine = message.mine;
    return Align(
      alignment: mine ? Alignment.centerRight : Alignment.centerLeft,
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 460),
        child: Container(
          padding: t.padding.symmetric(h: Space.md, v: Space.sm),
          decoration: BoxDecoration(
            color: mine ? t.brand.l800 : context.surface.l100,
            borderRadius: t.radius.all(Space.sm),
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                message.body ?? '',
                style: context.bodyStyle.copyWith(
                  color: mine ? context.surface.l50 : context.text.l950,
                ),
              ),
              if (!mine && !message.provenanceAgrees) ...[
                t.gap.y(Space.xxs),
                Text(
                  'delivered by a different device',
                  style: context.factLabelStyle,
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }
}

/// An invitation to a Space — a widget acted on, not read. The chatbot model:
/// a message that is a card with its own affordances.
class _InvitationCard extends StatelessWidget {
  const _InvitationCard({required this.message});

  final ChatMessageRow message;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final client = ClientScope.of(context);
    final entering = message.id != null &&
        ClientScope.watch(context)
            .inFlight
            .contains(ActionKeys.openInvitation(message.id!));
    return Align(
      alignment: message.mine ? Alignment.centerRight : Alignment.centerLeft,
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 460),
        child: Container(
          padding: t.padding.all(Space.md),
          decoration: BoxDecoration(
            color: context.surface.l100,
            borderRadius: t.radius.all(Space.sm),
            border: Border.all(color: t.brand.l800),
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Icon(AppIcons.inbox, size: t.font.md, color: t.brand.l800),
                  t.gap.x(Space.xs),
                  Text(
                    'Invitation to a Space',
                    style: context.bodyStyle.copyWith(
                      color: t.brand.l800,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                ],
              ),
              t.gap.y(Space.xs),
              Text(
                'Signed by the sender, and it verifies against its own '
                'Space. Opening it asks that Space to admit you.',
                style: context.proseStyle,
              ),
              t.gap.y(Space.sm),
              // Open enters the Space the invitation names. Decline is drawn
              // and inert: dismissing one letter needs somewhere to remember
              // the dismissal, and this backend has no such place yet — saying
              // so is better than a control that forgets.
              Row(
                children: [
                  Button(
                    key: const ValueKey('invitation-open'),
                    onPressed: (message.id == null || entering)
                        ? null
                        : () => client.dispatch(
                              ActionRequest.openInvitation(message: message.id!),
                            ),
                    label: 'Open',
                    variant: ButtonVariant.primary,
                    size: ButtonSize.sm,
                  ),
                  t.gap.x(Space.xs),
                  Button(
                    onPressed: null,
                    label: 'Decline',
                    tooltip: 'Declining is not available yet',
                    variant: ButtonVariant.ghost,
                    size: ButtonSize.sm,
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// The composer: an attach/emoji cluster, the line being typed, and a filled
/// send arrow. One rail, normalized heights. Enter sends; Shift+Enter is a
/// newline.
class _Composer extends StatelessWidget {
  const _Composer({
    required this.draft,
    required this.sending,
    required this.onSend,
  });

  final TextEditingController draft;
  final bool sending;
  final VoidCallback onSend;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Container(
      padding: t.padding.all(Space.md),
      decoration: BoxDecoration(
        border: Border(top: BorderSide(color: t.border.l500)),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.end,
        children: [
          // Attachments and emoji: a group apart from the input, as asked.
          // Both are drawn and inert, because neither exists yet: `Content` has
          // two variants, `Message` and `Invitation`, and no attachment among
          // them. Drawn rather than hidden so the composer keeps its shape, and
          // muted rather than silent so pressing one is not mistaken for a bug.
          _RailButton(
            semanticLabel: 'Attach a file (not available yet)',
            onTap: null,
            child: Icon(AppIcons.attachFile, size: t.font.md),
          ),
          t.gap.x(Space.xxs),
          _RailButton(
            semanticLabel: 'Emoji (not available yet)',
            onTap: null,
            child: Text('🙂', style: TextStyle(fontSize: t.font.md)),
          ),
          t.gap.x(Space.sm),
          Expanded(
            child: CallbackShortcuts(
              bindings: {
                const SingleActivator(LogicalKeyboardKey.enter): () {
                  if (!sending) onSend();
                },
              },
              child: Textarea(
                key: const ValueKey('chat-input'),
                controller: draft,
                hint: 'Message',
                minLines: 1,
                maxLines: 5,
              ),
            ),
          ),
          t.gap.x(Space.sm),
          _SendButton(enabled: !sending, onTap: onSend),
        ],
      ),
    );
  }
}

/// A square, normalized rail control for the composer's non-text affordances.
class _RailButton extends StatelessWidget {
  const _RailButton({
    required this.child,
    required this.semanticLabel,
    required this.onTap,
  });

  final Widget child;
  final String semanticLabel;

  /// Null draws the control muted and inert.
  ///
  /// This was a non-nullable `VoidCallback`, which made `() {}` the only way to
  /// express "does nothing" — and an empty closure is indistinguishable from a
  /// working button until you press it. A nullable callback moves the whole
  /// class of defect into the type: a control with nothing behind it now has to
  /// *say so*, and the analyzer will not let it pretend otherwise.
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final enabled = onTap != null;
    return Semantics(
      button: true,
      enabled: enabled,
      label: semanticLabel,
      child: GestureDetector(
        onTap: onTap,
        behavior: HitTestBehavior.opaque,
        child: SizedBox.square(
          dimension: 36,
          child: Center(
            child: IconTheme(
              data: IconThemeData(
                color: enabled ? context.text.l800 : context.text.l500,
              ),
              child: Opacity(opacity: enabled ? 1 : 0.5, child: child),
            ),
          ),
        ),
      ),
    );
  }
}

/// The send affordance: a filled circle with an up arrow, iMessage-style. Muted
/// while there is nothing to send.
class _SendButton extends StatelessWidget {
  const _SendButton({required this.enabled, required this.onTap});

  final bool enabled;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Semantics(
      button: true,
      label: 'Send',
      child: GestureDetector(
        onTap: enabled ? onTap : null,
        behavior: HitTestBehavior.opaque,
        child: Container(
          key: const ValueKey('chat-send'),
          width: 36,
          height: 36,
          decoration: BoxDecoration(
            color: enabled ? t.brand.l800 : context.surface.l200,
            shape: BoxShape.circle,
          ),
          child: Icon(
            AppIcons.arrowUpward,
            size: t.font.md,
            color: enabled ? context.surface.l50 : context.text.l700,
          ),
        ),
      ),
    );
  }
}

// ── Shared lookups and time formatting ──────────────────────────────────────

String _nameOf(CorrespondenceFacts? facts, String id) =>
    facts?.conversations
        .where((conversation) => conversation.peerId == id)
        .firstOrNull
        ?.peerName ??
    id;

bool _isAgent(CorrespondenceFacts? facts, String id) =>
    facts?.contacts
        .where((contact) => contact.id == id)
        .firstOrNull
        ?.isAgent ??
    false;

DateTime _at(ChatMessageRow message) =>
    DateTime.fromMillisecondsSinceEpoch(message.sentAt.toInt() * 1000);

bool _sameDay(DateTime a, DateTime b) =>
    a.year == b.year && a.month == b.month && a.day == b.day;

const List<String> _weekdays = [
  'Mon',
  'Tue',
  'Wed',
  'Thu',
  'Fri',
  'Sat',
  'Sun',
];
const List<String> _months = [
  'Jan',
  'Feb',
  'Mar',
  'Apr',
  'May',
  'Jun',
  'Jul',
  'Aug',
  'Sep',
  'Oct',
  'Nov',
  'Dec',
];

String _dayLabel(DateTime at) {
  final now = DateTime.now();
  final today = DateTime(now.year, now.month, now.day);
  final that = DateTime(at.year, at.month, at.day);
  final days = today.difference(that).inDays;
  if (days == 0) return 'Today';
  if (days == 1) return 'Yesterday';
  return '${_weekdays[at.weekday - 1]}, ${_months[at.month - 1]} ${at.day}';
}

String _timeLabel(DateTime at) {
  final hour = at.hour % 12 == 0 ? 12 : at.hour % 12;
  final minute = at.minute.toString().padLeft(2, '0');
  final period = at.hour < 12 ? 'AM' : 'PM';
  return '$hour:$minute $period';
}
