/// The front page: what this device serves, and what `Open` does about it.
///
/// ## Structured against the reference
///
/// The Spec names the Steam client as this client's reference shape, and its
/// library page is master–detail: a fixed rail of everything you have, and a
/// pane that says everything about the one you picked, with a single loud
/// primary action and a strip of facts under it. That composition is what was
/// taken. The component anatomy — a row's states, a card's padding, a button's
/// variant ladder — comes from covalence, which is why this file is mostly
/// arrangement and almost no drawing.
///
/// What was *not* taken is everything the reference fills its pane with that we
/// would have to invent: hero art, a play-time figure, an achievement bar. A
/// World has no capsule image and this client has no honest number for a stat
/// block, and a pane padded out with plausible-looking figures is the exact
/// defect the rest of this interface is written to avoid.
///
/// ## Selecting is not choosing
///
/// Picking a row reads nothing, places nothing and starts nothing — it moves
/// which of the facts already in hand are drawn. Listing is passive; `Open` is
/// the act. That is also why the selection is a draft held in this widget
/// rather than anything the core is told about.
library;

import 'package:covalence/covalence.dart' hide Surface;
// `ListTile` has its own entrypoint: its name collides with Material's, and
// covalence keeps it out of the main barrel rather than shadowing a name every
// Flutter app already has.
import 'package:covalence/listtile.dart';
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import '../shell/type.dart';

/// The rail's width. Pinned rather than proportional, which is the reference's
/// own choice and the right one: a rail is sized by the names in it, so a fifth
/// of a small window is a readable measure and a fifth of a maximised one is a
/// wide column of short words with the pane squeezed behind it.
const double kRailWidth = 208;

class LibrarySurface extends StatefulWidget {
  const LibrarySurface({super.key});

  @override
  State<LibrarySurface> createState() => _LibrarySurfaceState();
}

class _LibrarySurfaceState extends State<LibrarySurface> {
  /// Which row the pane is about.
  ///
  /// A row key rather than an index: the library is re-read on every refresh,
  /// and an index would silently follow whatever moved into that position.
  String? _selected;

  @override
  Widget build(BuildContext context) {
    final view = ClientScope.watch(context);
    final rows = view.library_;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text('Library', style: context.titleStyle),
        const SizedBox(height: 4),
        Text(
          'What this device serves. Open hands one to your browser.',
          style: context.proseStyle,
        ),
        const SizedBox(height: 20),
        Expanded(child: _body(context, rows)),
      ],
    );
  }

  Widget _body(BuildContext context, List<LibraryRow>? rows) {
    if (rows == null) {
      // Loading is not empty. A surface that drew the empty sentence while the
      // first read was still in flight would be claiming a fact it does not
      // have.
      return Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const Skeleton(width: kRailWidth, height: 28),
          const SizedBox(width: 20),
          const Expanded(child: Skeleton(height: 140)),
        ],
      );
    }
    if (rows.isEmpty) {
      return _Empty();
    }

    // Resolved rather than written back. Recording the fallback would turn
    // "nothing is selected yet" into "the first row was chosen", and the next
    // refresh would keep a choice the person never made.
    final showing = rows.firstWhere(
      (row) => row.key == _selected,
      orElse: () => rows.first,
    );

    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        SizedBox(
          width: kRailWidth,
          child: _Rail(
            rows: rows,
            showing: showing,
            onSelect: (row) => setState(() => _selected = row.key),
          ),
        ),
        const SizedBox(width: 20),
        Expanded(child: _Detail(showing: showing)),
      ],
    );
  }
}

class _Empty extends StatelessWidget {
  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        // Only sayable because the read succeeded and answered nothing.
        Text('This device serves no Worlds yet.', style: context.bodyStyle),
        const SizedBox(height: 4),
        // And the way out is named rather than left to be found. A person with
        // a fresh install and an invite in hand is exactly who is reading this,
        // and the flow they need cannot live in a World's head.
        Text(
          'Found a Space, or enter one from an invite, on the Spaces tab.',
          style: context.proseStyle,
        ),
      ],
    );
  }
}

class _Rail extends StatelessWidget {
  const _Rail({required this.rows, required this.showing, required this.onSelect});

  final List<LibraryRow> rows;
  final LibraryRow showing;
  final ValueChanged<LibraryRow> onSelect;

  @override
  Widget build(BuildContext context) {
    return ListView.separated(
      itemCount: rows.length,
      separatorBuilder: (_, __) => const SizedBox(height: 4),
      itemBuilder: (context, index) {
        final row = rows[index];
        return ListTile(
          selected: row.key == showing.key,
          onTap: () => onSelect(row),
          title: Text(
            _name(row),
            overflow: TextOverflow.ellipsis,
            // Dimmed when the Orbit is not up, which is the reference's own
            // device for "you have this but it is not installed" — and it
            // costs no colour the theme did not already answer.
            style: switch (row.placement) {
              PlacementView.placed => context.bodyStyle,
              PlacementView.vacant => context.proseStyle,
              PlacementView.unknown =>
                context.bodyStyle.copyWith(color: context.status.warning.l800),
            },
          ),
        );
      },
    );
  }
}

class _Detail extends StatelessWidget {
  const _Detail({required this.showing});

  final LibraryRow showing;

  @override
  Widget build(BuildContext context) {
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);

    final entryPath = showing.opensAt;
    final openKey =
        entryPath == null ? null : ActionKeys.open(showing.orbit, entryPath);
    final opening = openKey != null && view.inFlight.contains(openKey);

    return Card(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(_name(showing), style: context.headingStyle),
                    const SizedBox(height: 2),
                    Text(_subtitle(showing), style: context.proseStyle),
                  ],
                ),
              ),
              // The one act on this page, given the weight of one.
              Button(
                onPressed: entryPath == null || opening
                    ? null
                    : () => client.dispatch(
                          ActionRequest.open(
                            orbit: showing.orbit,
                            entryPath: entryPath,
                          ),
                        ),
                label: 'Open',
                isLoading: opening,
                variant: ButtonVariant.primary,
                size: ButtonSize.lg,
                // A row that cannot be opened says which kind of cannot. `/` is
                // not a guess to make on a World's behalf, and a Space that is
                // not running is a different case entirely — it opens at its
                // Orbit's own front door, which is what places it.
                tooltip: switch (showing.unopenable) {
                  Unopenable.unhosted =>
                    'This build hosts no head for that World, so there is '
                        'nothing to open.',
                  Unopenable.undeclared =>
                    'This World has not declared where to open it.',
                  null => 'Hand this World to your browser',
                },
              ),
            ],
          ),
          const SizedBox(height: 20),
          _Facts(showing: showing),
          ..._served(context, view),
        ],
      ),
    );
  }

  List<Widget> _served(BuildContext context, ClientView view) {
    // A browser head is bound to an identity and serves every Orbit that
    // identity has; an MCP head is authored against one.
    final serving = view.heads
        .where((head) => head.orbit == null || head.orbit == showing.orbit)
        .map((head) => head.origin)
        .nonNulls
        .toList();
    if (serving.isEmpty) return const [];
    return [
      const SizedBox(height: 20),
      Text('SERVED BY', style: context.factLabelStyle),
      const SizedBox(height: 4),
      // Where, and not how to get in: a head's URL carries a run credential and
      // the front page has no use for one. `Open` mints a single-use ticket of
      // its own, which is what that ceremony is for.
      for (final origin in serving) Text(origin, style: context.monoStyle),
    ];
  }
}

class _Facts extends StatelessWidget {
  const _Facts({required this.showing});

  final LibraryRow showing;

  @override
  Widget build(BuildContext context) {
    return Wrap(
      spacing: 32,
      runSpacing: 16,
      children: [
        _Fact(
          label: 'STATE',
          value: switch (showing.placement) {
            PlacementView.placed => 'running',
            PlacementView.vacant => 'not running',
            // "Not running" and "could not be asked" are different facts, and
            // only one of them is worth acting on.
            PlacementView.unknown => 'could not ask',
          },
          tone: showing.placement == PlacementView.unknown
              ? context.status.warning.l800
              : null,
        ),
        _Fact(label: 'LAST OPENED', value: _ago(showing.lastOpened)),
        _Fact(label: 'OPENS AT', value: showing.opensAt ?? 'nowhere'),
        _Fact(label: 'STORE', value: showing.store ?? 'not read', mono: true),
      ],
    );
  }
}

class _Fact extends StatelessWidget {
  const _Fact({required this.label, required this.value, this.tone, this.mono = false});

  final String label;
  final String value;
  final Color? tone;
  final bool mono;

  @override
  Widget build(BuildContext context) {
    final style = mono ? context.monoStyle : context.bodyStyle;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(label, style: context.factLabelStyle),
        const SizedBox(height: 2),
        Text(value, style: tone == null ? style : style.copyWith(color: tone)),
      ],
    );
  }
}

/// A row nobody named is drawn as unnamed. Substituting the id would put
/// something in the name column that is not a name, and a person cannot tell
/// that apart from a World genuinely called that.
String _name(LibraryRow row) => row.displayName ?? 'Unnamed Space';

/// What kind of row this is, and where it lives.
String _subtitle(LibraryRow row) => row.worldMount.isEmpty
    ? 'A Space in ${row.space}'
    : '${row.worldMount} in ${row.space}';

/// How long ago, at the scale a person reads.
///
/// `null` is *never opened* — the core sends an absence rather than a zero,
/// precisely so this cannot render it as 1 January 1970.
String _ago(BigInt? lastOpened) {
  if (lastOpened == null) return 'never';
  final then = DateTime.fromMillisecondsSinceEpoch(
    lastOpened.toInt() * 1000,
    isUtc: true,
  );
  final elapsed = DateTime.now().toUtc().difference(then);
  if (elapsed.isNegative) return 'in the future';
  if (elapsed.inMinutes < 1) return 'just now';
  if (elapsed.inHours < 1) return _plural(elapsed.inMinutes, 'minute');
  if (elapsed.inDays < 1) return _plural(elapsed.inHours, 'hour');
  return _plural(elapsed.inDays, 'day');
}

String _plural(int count, String unit) =>
    count == 1 ? '1 $unit ago' : '$count ${unit}s ago';
