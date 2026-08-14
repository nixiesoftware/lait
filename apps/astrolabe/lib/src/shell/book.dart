/// The address-book window.
///
/// Authored Cards for this identity. Drafts (search, dialog fields) live here.
/// Facts come from [ClientView.book]. Dispatch is the only write.
library;

import 'package:covalence/covalence.dart' hide Surface;
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

    return CallbackShortcuts(
      bindings: {
        // The header's Refresh control is gone; the shortcut is the whole
        // surface now, so it carries the same in-flight guard the button had.
        const SingleActivator(LogicalKeyboardKey.f5): () {
          if (!rereading) client.dispatch(const ActionRequest.refresh());
        },
        const SingleActivator(LogicalKeyboardKey.keyF, control: true):
            _openSearch,
        const SingleActivator(LogicalKeyboardKey.escape): _closeSearch,
      },
      child: Focus(
        autofocus: true,
        child: book == null
            ? const Empty(
                said: 'The book has not been read.',
                next:
                    'Press F5 to ask the daemon. Nothing is created on your behalf.',
              )
            : Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  _CanonicalCard(mine: mine.isEmpty ? null : mine.first),
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
                            itemBuilder: (context, index) =>
                                _CardRow(card: shown[index], all: book.cards),
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
  const _CanonicalCard({required this.mine});

  /// The claimed My Card, or null when the book — already read — holds none.
  final CardRow? mine;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final card = mine;
    final name = card == null ? 'No My Card.' : card.name;
    final status = card == null
        ? 'Claim one — nothing is implied from a name or a handle.'
        : 'Online';
    return Row(
      children: [
        t.box.square(
          // reason: the face copies the reference client's plate — pinned
          // like the caption controls, it sits against type rather than the
          // spacing rhythm.
          TokenEscape.rawSize(56),
          child: DecoratedBox(
            decoration: BoxDecoration(
              color: context.surface.l200,
              border: Border.all(
                color: context.border.l500,
                width: t.stroke.xxs,
              ),
              borderRadius: t.radius.all(Space.xxs),
            ),
            child: Center(
              child: card == null || card.name.isEmpty
                  ? Icon(AppIcons.person, color: context.text.l700)
                  : Text(
                      card.name.substring(0, 1).toUpperCase(),
                      style: context.headingStyle,
                    ),
            ),
          ),
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

class _CardRow extends StatelessWidget {
  const _CardRow({required this.card, required this.all});

  final CardRow card;
  final List<CardRow> all;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    bool busy(String key) => view.inFlight.contains(key);

    return Card(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(card.name, style: context.headingStyle),
              ),
              if (card.selfClaim)
                const Badge(label: 'My Card', variant: BadgeVariant.solid),
            ],
          ),
          t.gap.y(Space.xxs),
          Text(card.card, style: context.monoStyle),
          if (card.note.isNotEmpty) ...[
            t.gap.y(Space.sm),
            Text(card.note, style: context.bodyStyle),
          ],
          if (card.handles.isNotEmpty) ...[
            t.gap.y(Space.sm),
            for (final handle in card.handles)
              Padding(
                padding: t.padding.only(bottom: Space.xxs),
                child: Row(
                  children: [
                    Expanded(
                      child: Text(handle, style: context.monoStyle),
                    ),
                    Button(
                      onPressed: busy(ActionKeys.bookUnlink(card.card))
                          ? null
                          : () => client.dispatch(
                                ActionRequest.bookUnlink(
                                  card: card.card,
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
          t.gap.y(Space.md),
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
    );
  }
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
