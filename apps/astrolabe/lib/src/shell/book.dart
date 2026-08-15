/// The address-book window.
///
/// Authored Cards for this identity. Drafts (search, dialog fields) live here.
/// Facts come from [ClientView.book]. Dispatch is the only write.
library;

import 'package:covalence/covalence.dart' hide Image, Surface;
import 'package:flutter/material.dart' show MaterialApp, Scaffold, ThemeMode;
import 'package:flutter/services.dart' show LogicalKeyboardKey;
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import '../surfaces/page.dart';
import '../surfaces/surfaces.dart' show pageMargin;
import 'caption.dart' show kCaptionWidth;
import 'face.dart';
import 'host.dart';
import 'person.dart';
import 'theme.dart';
import 'type.dart';
import 'window.dart';

/// The book is a permanently portrait window — a rolodex, not a workspace.
/// The opening shape copies the reference client's friends panel (~370×760).
/// Its width ceiling stays below its height floor, so no drag, snap, or
/// shortcut can hand it a landscape shape: width ≤ 440 < 600 ≤ height at
/// every size the OS can grant. Maximise is refused at the HWND, not merely
/// undrawn.
const Size _bookOpening = Size(370, 760);
const Size _bookNarrowest = Size(320, 600);
const double _bookWidest = 440;

class BookApp extends StatelessWidget {
  const BookApp({super.key, required this.client});

  final Client client;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Address book',
      debugShowCheckedModeBanner: false,
      theme: astrolabeTheme(Brightness.light),
      darkTheme: astrolabeTheme(Brightness.dark),
      themeMode: ThemeMode.dark,
      home: ClientScope(
        client: client,
        child: Scaffold(
          body: AstrolabeWindowFrame.secondary(
            // The chrome carries no name and no band: the canonical card IS
            // this window's identity, so the caption merges into the body —
            // the card holds the top of the window with the two controls
            // floating beside it, and no rule between them. The OS-facing
            // title keeps the full name for the taskbar and Alt-Tab.
            title: '',
            nativeTitle: 'Address book — Astrolabe',
            nativeKey: bookWindowKey,
            size: _bookOpening,
            minimumSize: _bookNarrowest,
            maximumWidth: _bookWidest,
            maximisable: false,
            mergedCaption: true,
            dark: true,
            // The book is a client frame like the Library, not a document in
            // a page gutter: its CONTACTS strip owns the window edges, so the
            // margin is applied per section inside BookPage rather than as
            // one blanket wrapper here. It carries no operational metrics
            // row — that record lives on the main window.
            body: const BookPage(),
          ),
        ),
      ),
    );
  }
}

/// List, search, edit, delete, merge, My Card.
///
/// Search and dialog fields are drafts. The book itself is the view.
class BookPage extends StatefulWidget {
  const BookPage({super.key});

  @override
  State<BookPage> createState() => _BookPageState();
}

class _BookPageState extends State<BookPage> {
  String _query = '';
  bool _searching = false;

  /// The card whose profile subsurface is open, or null for the list. Held as
  /// an id, never a row: the row is re-read from the book every frame, so a
  /// profile can never show a card the book no longer holds.
  String? _profile;
  final FocusNode _searchFocus = FocusNode();

  @override
  void dispose() {
    _searchFocus.dispose();
    super.dispose();
  }

  void _openSearch() {
    setState(() => _searching = true);
    _searchFocus.requestFocus();
  }

  void _closeSearch() {
    if (!_searching) return;
    setState(() {
      _searching = false;
      _query = '';
    });
  }

  void _openProfile(String card) {
    setState(() => _profile = card);
  }

  void _closeProfile() {
    if (_profile == null) return;
    setState(() => _profile = null);
  }

  /// Escape peels one layer: the profile first, then the search draft.
  void _dismiss() {
    if (_profile != null) {
      _closeProfile();
      return;
    }
    _closeSearch();
  }

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    final book = view.book;
    final rereading = view.inFlight.contains(ActionKeys.refresh);
    bool busy(String key) => view.inFlight.contains(key);
    final mine = book?.cards.where((card) => card.selfClaim).toList() ?? const [];
    final shown = _filtered(book?.cards ?? const []);
    // Presence parts the list, not kind: everyone reachable — or not askable —
    // sits together, ordered by how present they are, and only the measured
    // absence gets a section of its own. An unmeasured card stays up top:
    // "could not be asked" is not a lesser Offline.
    final offline = shown
        .where((card) => card.presence == PresenceView.offline)
        .toList();
    final contacts = [
      ...shown.where((card) => card.presence == PresenceView.online),
      ...shown.where((card) => card.presence == PresenceView.away),
      ...shown.where((card) => card.presence == null),
    ];
    // The profile is re-read from the book every build: a deleted or merged
    // card falls back to the list on its own, never a stale page.
    CardRow? profiled;
    if (_profile != null && book != null) {
      for (final row in book.cards) {
        if (row.card == _profile) {
          profiled = row;
          break;
        }
      }
    }

    return CallbackShortcuts(
      bindings: {
        // The header's Refresh control is gone; the shortcut is the whole
        // surface now, so it carries the same in-flight guard the button had.
        const SingleActivator(LogicalKeyboardKey.f5): () {
          if (!rereading) client.dispatch(const ActionRequest.refresh());
        },
        const SingleActivator(LogicalKeyboardKey.keyF, control: true):
            _openSearch,
        const SingleActivator(LogicalKeyboardKey.escape): _dismiss,
      },
      child: Focus(
        autofocus: true,
        // The unread state and the profile are documents and keep the page
        // gutter; the list is a client frame whose strip owns the window
        // edges, so its sections carry the gutter themselves.
        child: book == null
            ? Padding(
                padding: pageMargin(t),
                child: const Empty(
                  said: 'The book has not been read.',
                  next:
                      'Press F5 to ask the daemon. Nothing is created on your behalf.',
                ),
              )
            : profiled != null
                ? Padding(
                    padding: pageMargin(t),
                    child: _ProfilePage(
                      card: profiled,
                      all: book.cards,
                      onBack: _closeProfile,
                    ),
                  )
                : Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Padding(
                    padding: t.padding.fromLTRB(
                      Space.xl3,
                      Space.xl,
                      Space.xl3,
                      Space.zero,
                    ),
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: [
                        _CanonicalCard(
                          mine: mine.isEmpty ? null : mine.first,
                          onOpen: mine.isEmpty
                              ? null
                              : () => _openProfile(mine.first.card),
                        ),
                        if (book.migrationPending > 0) ...[
                          t.gap.y(Space.md),
                          Text(
                            '${book.migrationPending} alias selector(s) still pending. '
                            'They were not turned into Cards.',
                            style: context.labelStyle,
                          ),
                        ],
                        if (book.suggestions.isNotEmpty) ...[
                          t.gap.y(Space.xl),
                          _SuggestionBand(
                            suggestions: book.suggestions,
                            busy: busy,
                            client: client,
                          ),
                        ],
                      ],
                    ),
                  ),
                  t.gap.y(Space.xl3),
                  // The reference client's panel head: a full-bleed strip —
                  // a raised band running edge to edge, with a hairline at
                  // each border. Its inner padding is the same gutter rung
                  // the sections use, so the title sits on the content line.
                  // Search lives inside the strip: a button first, a field
                  // second, and Escape closes it and clears the draft.
                  Container(
                    padding: t.padding.symmetric(h: Space.xl3, v: Space.xs),
                    decoration: BoxDecoration(
                      color: context.surface.l100,
                      border: t.stroke.edge(
                        top: context.border.l500,
                        bottom: context.border.l500,
                      ),
                    ),
                    child: _searching
                        ? Input(
                            search: true,
                            hint: 'Search cards',
                            size: InputSize.sm,
                            focusNode: _searchFocus,
                            autofocus: true,
                            onChanged: (value) =>
                                setState(() => _query = value),
                            // Cancel is a control, not only a key: it closes
                            // the field and clears the draft, same as Escape.
                            trailing: Button(
                              onPressed: _closeSearch,
                              icon: AppIcons.close,
                              semanticLabel: 'Cancel search',
                              variant: ButtonVariant.ghost,
                              size: ButtonSize.iconSm,
                              tooltip: 'Cancel search (Esc)',
                            ),
                          )
                        : Row(
                            children: [
                              Expanded(
                                child: Text(
                                  'CONTACTS',
                                  style: context.factLabelStyle.copyWith(
                                    color: context.text.l900,
                                    fontWeight: FontWeight.w700,
                                  ),
                                ),
                              ),
                              Button(
                                onPressed: _openSearch,
                                icon: AppIcons.search,
                                semanticLabel: 'Search cards',
                                variant: ButtonVariant.ghost,
                                size: ButtonSize.iconSm,
                                tooltip: 'Search cards (Ctrl+F)',
                              ),
                            ],
                          ),
                  ),
                  Expanded(
                    child: shown.isEmpty
                        ? Padding(
                            padding: pageMargin(t),
                            child: Empty(
                              said: book.cards.isEmpty
                                  ? 'No cards.'
                                  : 'No cards match that search.',
                              next: book.cards.isEmpty
                                  ? 'The book is this identity\'s, even with no Space open.'
                                  : 'Clear the search to see every Card.',
                            ),
                          )
                        // Present above, offline below, one rule between —
                        // the reference client's parted friends list. A book
                        // with nobody measured offline draws no rule.
                        : ListView(
                            padding: t.padding.fromLTRB(
                              Space.xl3,
                              Space.lg,
                              Space.xl3,
                              Space.xl3,
                            ),
                            children: [
                              if (contacts.isNotEmpty) ...[
                                // The count sits on the present section
                                // alone: how many are around is the number
                                // worth knowing; the absent need no tally.
                                _SectionHead(
                                  label: 'Contacts',
                                  count: contacts.length,
                                ),
                                t.gap.y(Space.md),
                                for (final row in contacts) ...[
                                  _PersonRow(
                                    card: row,
                                    onOpen: () => _openProfile(row.card),
                                  ),
                                  t.gap.y(Space.lg),
                                ],
                              ],
                              if (contacts.isNotEmpty &&
                                  offline.isNotEmpty) ...[
                                const Separator(),
                                t.gap.y(Space.lg),
                              ],
                              if (offline.isNotEmpty) ...[
                                const _SectionHead(label: 'Offline'),
                                t.gap.y(Space.md),
                                for (final row in offline) ...[
                                  _PersonRow(
                                    card: row,
                                    onOpen: () => _openProfile(row.card),
                                  ),
                                  t.gap.y(Space.lg),
                                ],
                              ],
                            ],
                          ),
                  ),
                ],
              ),
      ),
    );
  }

  List<CardRow> _filtered(List<CardRow> cards) {
    final q = _query.trim().toLowerCase();
    if (q.isEmpty) return cards;
    return cards
        .where(
          (card) =>
              card.name.toLowerCase().contains(q) ||
              card.note.toLowerCase().contains(q) ||
              card.card.toLowerCase().contains(q) ||
              card.handles.any((handle) => handle.toLowerCase().contains(q)),
        )
        .toList();
  }
}

/// The canonical card: how an identity is presented anywhere a person
/// appears — a picture, a name, a status. The book leads with your own.
///
/// The picture is a placeholder (a monogram, or a person mark when there is
/// nothing to monogram) until Cards carry an image. The status line is
/// derived, never asserted: measured presence on the card's own handles when
/// a Space answered, else the local fact — this identity's daemon answering
/// this very read. When even that is absent, the line is absent too.
class _CanonicalCard extends StatelessWidget {
  const _CanonicalCard({required this.mine, this.onOpen});

  /// The claimed My Card, or null when the book — already read — holds none.
  final CardRow? mine;

  /// Opens My Card's profile subsurface on double-click, like any row.
  final VoidCallback? onOpen;

  @override
  Widget build(BuildContext context) {
    final card = mine;
    final hostAnswered = ClientScope.watch(context).host != null;
    final name = card == null ? 'No My Card.' : card.name;
    final status = card == null
        ? 'Claim one — nothing is implied from a name or a handle.'
        : presenceLabel(card.presence) ?? (hostAnswered ? 'Online' : null);
    return GestureDetector(
      behavior: HitTestBehavior.opaque,
      onDoubleTap: onOpen,
      child: _canonicalRow(context, card, name, status),
    );
  }

  Widget _canonicalRow(
    BuildContext context,
    CardRow? card,
    String name,
    String? status,
  ) {
    final t = context.tokens;
    return Row(
      children: [
        FacePlate(
          picture: card?.picture,
          name: card?.name ?? '',
          size: 56,
        ),
        t.gap.x(Space.lg),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                name,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: context.headingStyle,
              ),
              if (status != null) ...[
                t.gap.y(Space.xxs),
                Text(
                  status,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: context.labelStyle,
                ),
              ],
            ],
          ),
        ),
        // The card shares the window's top band with the floating minimise
        // and close controls, so it cedes their corner — two caption widths
        // — rather than running its text underneath them.
        t.box.width(
          TokenEscape.rawSize(kCaptionWidth * 2),
          child: const SizedBox(),
        ),
      ],
    );
  }
}

/// The marking a list section wears — the reference client's "In Game" /
/// "Online Friends (7)" shape: a quiet sentence-case label with the count
/// dimmer beside it. The list is parted by presence alone: Contacts (present
/// or unmeasured) above, Offline — the measured absence — below.
class _SectionHead extends StatelessWidget {
  const _SectionHead({required this.label, this.count});

  final String label;
  final int? count;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Row(
      children: [
        Text(
          label,
          style: context.labelStyle.copyWith(
            color: context.text.l800,
            fontWeight: FontWeight.w600,
          ),
        ),
        if (count != null) ...[
          t.gap.x(Space.xs),
          Text('($count)', style: context.labelStyle),
        ],
      ],
    );
  }
}

/// A person in the list — the canonical [PersonTile], with the book's own
/// affordances composed around it. Everything a card carries — its handles
/// and every action on it — lives on the profile page, and double-clicking
/// the row opens it in this window.
class _PersonRow extends StatelessWidget {
  const _PersonRow({required this.card, required this.onOpen});

  final CardRow card;
  final VoidCallback onOpen;

  @override
  Widget build(BuildContext context) {
    return Semantics(
      button: true,
      label: 'Open the profile of ${card.name}',
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onDoubleTap: onOpen,
        // The canonical tile draws the person; this row only composes the
        // book's own affordances around it — the gesture and the claim.
        child: PersonTile(
          name: card.name,
          picture: card.picture,
          presence: card.presence,
          agent: _agentCard(card),
          note: card.note,
          trailing: card.selfClaim
              ? const Badge(label: 'My Card', variant: BadgeVariant.solid)
              : null,
        ),
      ),
    );
  }
}

/// The profile page — the subsurface a card's whole truth lives on, rendered
/// in the parent window in place of the list. Back (or Escape) returns.
class _ProfilePage extends StatelessWidget {
  const _ProfilePage({
    required this.card,
    required this.all,
    required this.onBack,
  });

  final CardRow card;
  final List<CardRow> all;
  final VoidCallback onBack;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    bool busy(String key) => view.inFlight.contains(key);

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Button(
              onPressed: onBack,
              icon: AppIcons.arrowBack,
              semanticLabel: 'Back to the book',
              variant: ButtonVariant.ghost,
              size: ButtonSize.iconSm,
              tooltip: 'Back (Esc)',
            ),
          ],
        ),
        t.gap.y(Space.lg),
        Row(
          children: [
            FacePlate(picture: card.picture, name: card.name, size: 56),
            t.gap.x(Space.lg),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      Expanded(
                        child: Text(
                          card.name,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: context.headingStyle,
                        ),
                      ),
                      if (card.selfClaim)
                        const Badge(
                          label: 'My Card',
                          variant: BadgeVariant.solid,
                        ),
                    ],
                  ),
                  if (card.note.isNotEmpty) ...[
                    t.gap.y(Space.xxs),
                    Text(card.note, style: context.labelStyle),
                  ],
                ],
              ),
            ),
          ],
        ),
        t.gap.y(Space.lg),
        Expanded(
          child: ListView(
            children: [
              _HandleSection(
                label: 'ADDRESSES',
                handles: card.addresses,
                card: card.card,
              ),
              _HandleSection(
                label: 'DEVICES',
                handles: card.devices,
                card: card.card,
              ),
              _HandleSection(
                label: 'AGENTS',
                handles: card.agents,
                card: card.card,
              ),
              t.gap.y(Space.xl),
              Wrap(
                spacing: t.size.sm,
                runSpacing: t.size.xs,
                children: [
                  Button(
                    onPressed: busy(ActionKeys.bookPut(card.card))
                        ? null
                        : () => _edit(context, card),
                    label: 'Edit',
                    variant: ButtonVariant.ghost,
                    size: ButtonSize.sm,
                  ),
                  Button(
                    onPressed: busy(ActionKeys.bookSetPicture(card.card))
                        ? null
                        : () => _setPicture(context, card),
                    label: 'Set picture',
                    variant: ButtonVariant.ghost,
                    size: ButtonSize.sm,
                  ),
                  if (card.picture != null)
                    Button(
                      onPressed: busy(ActionKeys.bookSetPicture(card.card))
                          ? null
                          : () => client.dispatch(
                                ActionRequest.bookSetPicture(
                                  card: card.card,
                                  path: null,
                                ),
                              ),
                      label: 'Clear picture',
                      variant: ButtonVariant.ghost,
                      size: ButtonSize.sm,
                    ),
                  if (!card.selfClaim)
                    Button(
                      onPressed: busy(ActionKeys.bookClaim(card.card))
                          ? null
                          : () => client.dispatch(
                                ActionRequest.bookClaimSelf(card: card.card),
                              ),
                      label: 'Claim as My Card',
                      variant: ButtonVariant.ghost,
                      size: ButtonSize.sm,
                    ),
                  Button(
                    onPressed: busy(ActionKeys.bookLink(card.card))
                        ? null
                        : () => _link(context, card),
                    label: 'Add handle',
                    variant: ButtonVariant.ghost,
                    size: ButtonSize.sm,
                  ),
                  if (all.length > 1)
                    Button(
                      onPressed: () => _merge(context, card, all),
                      label: 'Merge',
                      variant: ButtonVariant.ghost,
                      size: ButtonSize.sm,
                    ),
                  Button(
                    onPressed: busy(ActionKeys.bookDelete(card.card))
                        ? null
                        : () => _delete(context, card),
                    label: 'Delete',
                    variant: ButtonVariant.destructiveGhost,
                    size: ButtonSize.sm,
                  ),
                ],
              ),
            ],
          ),
        ),
      ],
    );
  }
}

/// The canonical group the daemon files an agent's card under — part of the
/// book's wire vocabulary (`lait::control::AGENT_GROUP`), not a display
/// string. Provisioning stamps it, and the roster decoration heals older
/// books, because an agent's card carries an ordinary `actor:` address: at
/// the identity layer agents are members, so the handles alone cannot part
/// them from people.
const String _agentGroup = 'Agents';

/// An agent's own card: filed under the agent group, or carrying nothing but
/// `agent:` spellings. A person's card may list co-located agents; what keeps
/// it a person's is that an address or a device anchors it. Worn as the AI
/// mark on the card's row — never as a section: what an identity is and
/// whether it is here are different axes, and the list is parted only by the
/// second.
bool _agentCard(CardRow card) =>
    card.groups.contains(_agentGroup) ||
    (card.agents.isNotEmpty && card.addresses.isEmpty && card.devices.isEmpty);

/// One phone-book section: a label and its rows, each unlinkable. Absent
/// kinds draw nothing — a card with no devices has no DEVICES heading.
class _HandleSection extends StatelessWidget {
  const _HandleSection({
    required this.label,
    required this.handles,
    required this.card,
  });

  final String label;
  final List<String> handles;
  final String card;

  @override
  Widget build(BuildContext context) {
    if (handles.isEmpty) return const SizedBox.shrink();
    final t = context.tokens;
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    final busy = view.inFlight.contains(ActionKeys.bookUnlink(card));
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        t.gap.y(Space.md),
        Text(label, style: context.factLabelStyle),
        t.gap.y(Space.xxs),
        for (final handle in handles)
          Padding(
            padding: t.padding.only(bottom: Space.xxs),
            child: Row(
              children: [
                Expanded(
                  child: Text(handle, style: context.monoStyle),
                ),
                Button(
                  onPressed: busy
                      ? null
                      : () => client.dispatch(
                            ActionRequest.bookUnlink(
                              card: card,
                              handle: handle,
                            ),
                          ),
                  label: 'Unlink',
                  semanticLabel: 'Unlink $handle',
                  variant: ButtonVariant.ghost,
                  size: ButtonSize.sm,
                ),
              ],
            ),
          ),
      ],
    );
  }
}

Future<void> _setPicture(BuildContext context, CardRow card) async {
  final path = TextEditingController();
  final saved = await showAppDialog<bool>(
    context: context,
    builder: (ctx) => DialogContent(
      children: [
        DialogHeader(
          title: DialogTitle('Set the picture on ${card.name}'),
          description: const DialogDescription(
            'A PNG, JPEG, or WebP on this machine. The book stores the '
            'picture itself — keep it face-sized, not a photo.',
          ),
        ),
        Input(
          key: const ValueKey('book-picture-path'),
          controller: path,
          label: 'Path',
          mono: true,
          autofocus: true,
        ),
        DialogFooter(
          children: [
            Button(
              label: 'Cancel',
              variant: ButtonVariant.outline,
              onPressed: () => Navigator.pop(ctx, false),
            ),
            Button(
              label: 'Set picture',
              onPressed: () => Navigator.pop(ctx, true),
            ),
          ],
        ),
      ],
    ),
  );
  final dest = path.text.trim();
  path.dispose();
  if (saved != true || dest.isEmpty || !context.mounted) return;
  ClientScope.of(context).dispatch(
    ActionRequest.bookSetPicture(card: card.card, path: dest),
  );
}

Future<void> _edit(BuildContext context, CardRow? existing) async {
  final name = TextEditingController(text: existing?.name ?? '');
  final note = TextEditingController(text: existing?.note ?? '');
  final saved = await showAppDialog<bool>(
    context: context,
    builder: (ctx) => DialogContent(
      children: [
        DialogHeader(
          title: DialogTitle(existing == null ? 'New card' : 'Edit card'),
          description: const DialogDescription(
            'A name is an authored label. It never selects an authority target.',
          ),
        ),
        Input(
          key: const ValueKey('book-name'),
          controller: name,
          label: 'Name',
          autofocus: true,
        ),
        Input(
          key: const ValueKey('book-note'),
          controller: note,
          label: 'Note',
        ),
        DialogFooter(
          children: [
            Button(
              label: 'Cancel',
              variant: ButtonVariant.outline,
              onPressed: () => Navigator.pop(ctx, false),
            ),
            Button(
              label: 'Save',
              onPressed: () => Navigator.pop(ctx, true),
            ),
          ],
        ),
      ],
    ),
  );
  final trimmed = name.text.trim();
  name.dispose();
  final noteText = note.text.trim();
  note.dispose();
  if (saved != true || trimmed.isEmpty || !context.mounted) return;
  ClientScope.of(context).dispatch(
    ActionRequest.bookPut(
      card: existing?.card,
      name: trimmed,
      note: noteText.isEmpty ? null : noteText,
    ),
  );
}

Future<void> _link(BuildContext context, CardRow card) async {
  final handle = TextEditingController();
  final saved = await showAppDialog<bool>(
    context: context,
    builder: (ctx) => DialogContent(
      children: [
        DialogHeader(
          title: DialogTitle('Add a handle to ${card.name}'),
          description: const DialogDescription(
            'Wire spelling: a device id, actor:space:actor, or agent:hash:name.',
          ),
        ),
        Input(
          controller: handle,
          label: 'Handle',
          mono: true,
          autofocus: true,
        ),
        DialogFooter(
          children: [
            Button(
              label: 'Cancel',
              variant: ButtonVariant.outline,
              onPressed: () => Navigator.pop(ctx, false),
            ),
            Button(
              label: 'Link',
              onPressed: () => Navigator.pop(ctx, true),
            ),
          ],
        ),
      ],
    ),
  );
  final raw = handle.text.trim();
  handle.dispose();
  if (saved != true || raw.isEmpty || !context.mounted) return;
  ClientScope.of(context).dispatch(
    ActionRequest.bookLink(card: card.card, handle: raw),
  );
}

Future<void> _merge(
  BuildContext context,
  CardRow from,
  List<CardRow> all,
) async {
  final others = all.where((card) => card.card != from.card).toList();
  if (others.isEmpty) return;
  var into = others.first.card;
  final typed = TextEditingController();
  final saved = await showAppDialog<bool>(
    context: context,
    builder: (ctx) => StatefulBuilder(
      builder: (ctx, setLocal) => DialogContent(
        children: [
          DialogHeader(
            title: DialogTitle('Merge ${from.name}?'),
            description: DialogDescription(
              'This Card is absorbed into another. Type ${from.name} to confirm.',
            ),
          ),
          Select<String>(
            value: into,
            onValueChange: (value) {
              if (value != null) setLocal(() => into = value);
            },
            trigger: const SelectTrigger(
              child: SelectValue(placeholder: 'Merge into…'),
            ),
            child: SelectContent(
              children: [
                for (final card in others)
                  SelectItem(
                    value: card.card,
                    label: card.name,
                    child: Text(card.name),
                  ),
              ],
            ),
          ),
          Input(
            key: const ValueKey('book-confirm-name'),
            controller: typed,
            hint: from.name,
            semanticLabel: 'Type this card\'s name to confirm',
            onChanged: (_) => setLocal(() {}),
          ),
          DialogFooter(
            children: [
              Button(
                label: 'Cancel',
                variant: ButtonVariant.outline,
                onPressed: () => Navigator.pop(ctx, false),
              ),
              Button(
                label: 'Merge',
                variant: ButtonVariant.destructive,
                onPressed: typed.text.trim() == from.name
                    ? () => Navigator.pop(ctx, true)
                    : null,
                tooltip: typed.text.trim() == from.name
                    ? null
                    : 'Type this card\'s name to confirm.',
              ),
            ],
          ),
        ],
      ),
    ),
  );
  typed.dispose();
  if (saved != true || !context.mounted) return;
  ClientScope.of(context).dispatch(
    ActionRequest.bookMerge(from: from.card, into: into),
  );
}

Future<void> _delete(BuildContext context, CardRow card) async {
  final typed = TextEditingController();
  final saved = await showAppDialog<bool>(
    context: context,
    builder: (ctx) => StatefulBuilder(
      builder: (ctx, setLocal) => DialogContent(
        children: [
          DialogHeader(
            title: DialogTitle('Delete ${card.name}?'),
            description: DialogDescription(
              'This cannot be undone. Type ${card.name} to confirm.',
            ),
          ),
          Input(
            key: const ValueKey('book-confirm-name'),
            controller: typed,
            hint: card.name,
            semanticLabel: 'Type this card\'s name to confirm',
            onChanged: (_) => setLocal(() {}),
          ),
          DialogFooter(
            children: [
              Button(
                label: 'Cancel',
                variant: ButtonVariant.outline,
                onPressed: () => Navigator.pop(ctx, false),
              ),
              Button(
                label: 'Delete',
                variant: ButtonVariant.destructive,
                icon: AppIcons.trash,
                onPressed: typed.text.trim() == card.name
                    ? () => Navigator.pop(ctx, true)
                    : null,
                tooltip: typed.text.trim() == card.name
                    ? null
                    : 'Type this card\'s name to confirm.',
              ),
            ],
          ),
        ],
      ),
    ),
  );
  typed.dispose();
  if (saved != true || !context.mounted) return;
  ClientScope.of(context).dispatch(
    ActionRequest.bookDelete(card: card.card),
  );
}

/// Staged suggestions from card-exchange files. Review is the only way into
/// the book: each row is accepted or dismissed, never silently applied.
class _SuggestionBand extends StatelessWidget {
  const _SuggestionBand({
    required this.suggestions,
    required this.busy,
    required this.client,
  });

  final List<SuggestionRow> suggestions;
  final bool Function(String key) busy;
  final Client client;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Card(
      child: Padding(
        padding: t.padding.all(Space.lg),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text(
              '${suggestions.length} suggested from files',
              style: context.headingStyle,
            ),
            t.gap.y(Space.xs),
            Text(
              'Nothing below is in the book until you accept it.',
              style: context.labelStyle,
            ),
            for (final suggestion in suggestions) ...[
              t.gap.y(Space.md),
              Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(suggestion.name, style: context.bodyStyle),
                        if (suggestion.note.isNotEmpty)
                          Text(suggestion.note, style: context.labelStyle),
                        for (final handle in suggestion.handles)
                          Text(handle, style: context.monoStyle),
                      ],
                    ),
                  ),
                  t.gap.x(Space.md),
                  Button(
                    onPressed: busy(ActionKeys.bookAccept(suggestion.suggestion))
                        ? null
                        : () => client.dispatch(
                              ActionRequest.bookAccept(
                                suggestion: suggestion.suggestion,
                              ),
                            ),
                    label: 'Accept',
                    size: ButtonSize.sm,
                  ),
                  t.gap.x(Space.sm),
                  Button(
                    onPressed:
                        busy(ActionKeys.bookDismiss(suggestion.suggestion))
                            ? null
                            : () => client.dispatch(
                                  ActionRequest.bookDismiss(
                                    suggestion: suggestion.suggestion,
                                  ),
                                ),
                    label: 'Dismiss',
                    variant: ButtonVariant.ghost,
                    size: ButtonSize.sm,
                  ),
                ],
              ),
            ],
          ],
        ),
      ),
    );
  }
}
