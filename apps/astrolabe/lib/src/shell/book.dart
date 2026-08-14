/// The address-book window.
///
/// Authored Cards for this identity. Drafts (search, dialog fields) live here.
/// Facts come from [ClientView.book]. Dispatch is the only write.
library;

import 'dart:convert' show base64Decode;
import 'dart:typed_data' show Uint8List;

import 'package:covalence/covalence.dart' hide Image, Surface;
import 'package:flutter/material.dart' show MaterialApp, Scaffold, ThemeMode;
import 'package:flutter/services.dart' show LogicalKeyboardKey;
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import '../surfaces/page.dart';
import '../surfaces/surfaces.dart' show pageMargin;
import 'host.dart';
import 'record.dart';
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
            // The chrome carries no name. The canonical card below IS this
            // window's identity, and a caption repeating "Address book" above
            // it was a label spending height on what the card already says.
            // The OS-facing title keeps the full name for the taskbar and
            // Alt-Tab.
            title: '',
            nativeTitle: 'Address book — Astrolabe',
            nativeKey: bookWindowKey,
            size: _bookOpening,
            minimumSize: _bookNarrowest,
            maximumWidth: _bookWidest,
            maximisable: false,
            dark: true,
            // The book is a conventional page, not the Library's client
            // frame: it keeps the shared page gutter the main window's
            // surfaces get from SurfacePage, and the same operational
            // record underneath — which is also the window's live region
            // for landed and refused writes.
            body: Column(
              children: [
                Expanded(
                  child: Builder(
                    builder: (context) => Padding(
                      padding: pageMargin(context.tokens),
                      child: const BookPage(),
                    ),
                  ),
                ),
                const OperationalBar(),
              ],
            ),
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
        child: book == null
            ? const Empty(
                said: 'The book has not been read.',
                next:
                    'Press F5 to ask the daemon. Nothing is created on your behalf.',
              )
            : profiled != null
                ? _ProfilePage(
                    card: profiled,
                    all: book.cards,
                    onBack: _closeProfile,
                  )
                : Column(
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
                  t.gap.y(Space.xl3),
                  // Search is a button first, a field second — the reference
                  // client's shape. Escape closes it and clears the draft.
                  if (_searching)
                    Input(
                      search: true,
                      hint: 'Search cards',
                      size: InputSize.sm,
                      focusNode: _searchFocus,
                      autofocus: true,
                      onChanged: (value) => setState(() => _query = value),
                      // Cancel is a control, not only a key: it closes the
                      // field and clears the draft, same as Escape.
                      trailing: Button(
                        onPressed: _closeSearch,
                        icon: AppIcons.close,
                        semanticLabel: 'Cancel search',
                        variant: ButtonVariant.ghost,
                        size: ButtonSize.iconSm,
                        tooltip: 'Cancel search (Esc)',
                      ),
                    )
                  else
                    Row(
                      mainAxisAlignment: MainAxisAlignment.end,
                      children: [
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
                  t.gap.y(Space.xl3),
                  Expanded(
                    child: shown.isEmpty
                        ? Empty(
                            said: book.cards.isEmpty
                                ? 'No cards.'
                                : 'No cards match that search.',
                            next: book.cards.isEmpty
                                ? 'The book is this identity\'s, even with no Space open.'
                                : 'Clear the search to see every Card.',
                          )
                        : ListView.separated(
                            itemCount: shown.length,
                            separatorBuilder: (_, __) => t.gap.y(Space.md),
                            itemBuilder: (context, index) => _PersonRow(
                              card: shown[index],
                              onOpen: () =>
                                  _openProfile(shown[index].card),
                            ),
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
/// nothing to monogram) until Cards carry an image. The status line is a
/// design placeholder too: presence is not plumbed into [BookFacts] yet, and
/// when it is, this line must come from a measurement — never a default.
class _CanonicalCard extends StatelessWidget {
  const _CanonicalCard({required this.mine, this.onOpen});

  /// The claimed My Card, or null when the book — already read — holds none.
  final CardRow? mine;

  /// Opens My Card's profile subsurface on double-click, like any row.
  final VoidCallback? onOpen;

  @override
  Widget build(BuildContext context) {
    final card = mine;
    final name = card == null ? 'No My Card.' : card.name;
    final status = card == null
        ? 'Claim one — nothing is implied from a name or a handle.'
        : 'Online';
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
    String status,
  ) {
    final t = context.tokens;
    return Row(
      children: [
        _FacePlate(
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
              t.gap.y(Space.xxs),
              Text(
                status,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: context.labelStyle,
              ),
            ],
          ),
        ),
      ],
    );
  }
}

/// A person in the list: the face and the name, nothing else. Everything a
/// card carries — its handles and every action on it — lives on the profile
/// page, and double-clicking the row opens it in this window.
class _PersonRow extends StatelessWidget {
  const _PersonRow({required this.card, required this.onOpen});

  final CardRow card;
  final VoidCallback onOpen;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Semantics(
      button: true,
      label: 'Open the profile of ${card.name}',
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onDoubleTap: onOpen,
        child: Card(
          child: Row(
            children: [
              _FacePlate(picture: card.picture, name: card.name, size: 40),
              t.gap.x(Space.md),
              Expanded(
                child: Text(
                  card.name,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: context.headingStyle,
                ),
              ),
              if (card.selfClaim)
                const Badge(label: 'My Card', variant: BadgeVariant.solid),
            ],
          ),
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
            _FacePlate(picture: card.picture, name: card.name, size: 56),
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

/// The face on a card: the stored picture when one was authored, else the
/// default — a monogram, or the person mark when there is nothing to
/// monogram. Boxed like the reference client's plates.
class _FacePlate extends StatelessWidget {
  const _FacePlate({
    required this.picture,
    required this.name,
    required this.size,
  });

  final String? picture;
  final String name;
  final double size;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final bytes = _pictureBytes(picture);
    return t.box.square(
      // reason: the face copies the reference client's plate — pinned like
      // the caption controls, it sits against type rather than the spacing
      // rhythm.
      TokenEscape.rawSize(size),
      child: DecoratedBox(
        decoration: BoxDecoration(
          color: context.surface.l200,
          border: Border.all(
            color: context.border.l500,
            width: t.stroke.xxs,
          ),
          borderRadius: t.radius.all(Space.xxs),
        ),
        child: bytes != null
            ? ClipRRect(
                borderRadius: t.radius.all(Space.xxs),
                child: Image.memory(
                  bytes,
                  fit: BoxFit.cover,
                  gaplessPlayback: true,
                ),
              )
            : Center(
                child: name.isEmpty
                    ? Icon(AppIcons.person, color: context.text.l700)
                    : Text(
                        name.substring(0, 1).toUpperCase(),
                        style: context.headingStyle,
                      ),
              ),
      ),
    );
  }
}

/// Decode the stored `<mime>;base64,<data>` form. The engine validated it at
/// write, so a miss here is a corrupt store answered with the default face —
/// never a crash in a list row.
Uint8List? _pictureBytes(String? stored) {
  if (stored == null) return null;
  final split = stored.indexOf(';base64,');
  if (split < 0) return null;
  try {
    return base64Decode(stored.substring(split + 8));
  } catch (_) {
    return null;
  }
}

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
