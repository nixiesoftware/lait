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

const Size _bookOpening = Size(800, 600);
const Size _bookNarrowest = Size(600, 400);

class BookApp extends StatefulWidget {
  const BookApp({super.key, required this.client});

  final Client client;

  @override
  State<BookApp> createState() => _BookAppState();
}

class _BookAppState extends State<BookApp> {
  ThemeMode _themeMode = ThemeMode.dark;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Address book',
      debugShowCheckedModeBanner: false,
      theme: astrolabeTheme(Brightness.light),
      darkTheme: astrolabeTheme(Brightness.dark),
      themeMode: _themeMode,
      home: ClientScope(
        client: widget.client,
        child: Scaffold(
          body: AstrolabeWindowFrame.secondary(
            title: 'Address book',
            nativeTitle: 'Address book — Astrolabe',
            nativeKey: bookWindowKey,
            size: _bookOpening,
            minimumSize: _bookNarrowest,
            dark: _themeMode == ThemeMode.dark,
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
                      child: BookPage(
                        themeMode: _themeMode,
                        onToggleTheme: () => setState(() {
                          _themeMode = _themeMode == ThemeMode.dark
                              ? ThemeMode.light
                              : ThemeMode.dark;
                        }),
                      ),
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

/// List, search, create, edit, delete, merge, My Card.
///
/// Search and dialog fields are drafts. The book itself is the view.
class BookPage extends StatefulWidget {
  const BookPage({
    super.key,
    required this.themeMode,
    required this.onToggleTheme,
  });

  final ThemeMode themeMode;
  final VoidCallback onToggleTheme;

  @override
  State<BookPage> createState() => _BookPageState();
}

class _BookPageState extends State<BookPage> {
  String _query = '';
  final FocusNode _searchFocus = FocusNode();

  @override
  void dispose() {
    _searchFocus.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    final book = view.book;
    final rereading = view.inFlight.contains(ActionKeys.refresh);
    final busyExport = view.inFlight.contains(ActionKeys.bookExport);
    final busyImport = view.inFlight.contains(ActionKeys.bookImport);
    final busyNewCard = view.inFlight.contains(ActionKeys.bookPutNew);
    bool busy(String key) => view.inFlight.contains(key);
    final mine = book?.cards.where((card) => card.selfClaim).toList() ?? const [];
    final shown = _filtered(book?.cards ?? const []);

    return CallbackShortcuts(
      bindings: {
        const SingleActivator(LogicalKeyboardKey.f5): () =>
            client.dispatch(const ActionRequest.refresh()),
        const SingleActivator(LogicalKeyboardKey.keyF, control: true): () =>
            _searchFocus.requestFocus(),
      },
      child: Focus(
        autofocus: true,
        child: SurfaceScaffold(
          title: 'Address book',
      prose:
          'Authored Cards for this identity. Names never select an authority target.',
      trailing: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Button(
            onPressed: rereading
                ? null
                : () => client.dispatch(const ActionRequest.refresh()),
            icon: AppIcons.refresh,
            semanticLabel: 'Refresh',
            isLoading: rereading,
            variant: ButtonVariant.ghost,
            size: ButtonSize.iconSm,
            tooltip: 'Read this machine again',
          ),
          t.gap.x(Space.xs),
          Button(
            onPressed: widget.onToggleTheme,
            icon: widget.themeMode == ThemeMode.dark
                ? AppIcons.toggleOn
                : AppIcons.toggleOff,
            semanticLabel: widget.themeMode == ThemeMode.dark
                ? 'Use light theme'
                : 'Use dark theme',
            variant: ButtonVariant.ghost,
            size: ButtonSize.iconSm,
          ),
          t.gap.x(Space.sm),
          Button(
            onPressed: book == null || busyExport
                ? null
                : () => _bundle(context, export: true),
            label: 'Export',
            variant: ButtonVariant.ghost,
            size: ButtonSize.sm,
          ),
          Button(
            onPressed: busyImport ? null : () => _bundle(context, export: false),
            label: 'Import',
            variant: ButtonVariant.ghost,
            size: ButtonSize.sm,
          ),
          t.gap.x(Space.sm),
          Button(
            onPressed: busyNewCard ? null : () => _edit(context, null),
            icon: AppIcons.personAdd,
            label: 'New card',
            size: ButtonSize.sm,
          ),
        ],
      ),
      child: book == null
          ? const Empty(
              said: 'The book has not been read.',
              next: 'Refresh to ask the daemon. Nothing is created on your behalf.',
            )
          : Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                _MyCardBand(mine: mine),
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
                Input(
                  search: true,
                  hint: 'Search cards',
                  size: InputSize.sm,
                  focusNode: _searchFocus,
                  onChanged: (value) => setState(() => _query = value),
                ),
                t.gap.y(Space.xl3),
                Expanded(
                  child: shown.isEmpty
                      ? Empty(
                          said: book.cards.isEmpty
                              ? 'No cards.'
                              : 'No cards match that search.',
                          next: book.cards.isEmpty
                              ? 'Create one. The book is this identity\'s, even with no Space open.'
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

class _MyCardBand extends StatelessWidget {
  const _MyCardBand({required this.mine});

  final List<CardRow> mine;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    if (mine.isEmpty) {
      return Card(
        variant: CardVariant.muted,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('MY CARD', style: context.factLabelStyle),
            t.gap.y(Space.sm),
            Text('No My Card.', style: context.bodyStyle),
            t.gap.y(Space.xxs),
            Text(
              'Claim one — nothing is implied from a name or a handle.',
              style: context.labelStyle,
            ),
          ],
        ),
      );
    }
    final card = mine.first;
    return Card(
      variant: CardVariant.muted,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text('MY CARD', style: context.factLabelStyle),
          t.gap.y(Space.sm),
          Text(card.name, style: context.headingStyle),
          t.gap.y(Space.xxs),
          Text(card.card, style: context.monoStyle),
        ],
      ),
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

Future<void> _bundle(BuildContext context, {required bool export}) async {
  final path = TextEditingController();
  final saved = await showAppDialog<bool>(
    context: context,
    builder: (ctx) => DialogContent(
      children: [
        DialogHeader(
          title: DialogTitle(export ? 'Export cards' : 'Import cards'),
          description: DialogDescription(
            export
                ? 'A suggestion file. Local-agent handles and My Card do not travel.'
                : 'New Cards only. Existing Cards and My Card are left alone.',
          ),
        ),
        Input(
          key: const ValueKey('book-bundle-path'),
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
              label: export ? 'Export' : 'Import',
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
    export
        ? ActionRequest.bookExport(path: dest)
        : ActionRequest.bookImport(path: dest),
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
