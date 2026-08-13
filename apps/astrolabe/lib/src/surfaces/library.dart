/// The front page: what this device serves, and what `Open` does about it.
///
/// ## Structured against the reference
///
/// The Spec names the Steam client as this client's reference shape, and its
/// library is master–detail: a rail of everything you have, and a pane about
/// the one you picked — a hero plate, then a row carrying the single primary
/// action beside a strip of facts, then the places you can jump to, then
/// whatever the thing itself has to say. That composition is what was taken.
///
/// ## The World fills the template; the client only draws it
///
/// Steam's hero is art the game shipped. A World has no art, and it must not
/// have any: the display contract is deliberately a glyph and a colour rather
/// than an image path, because a Library that fetched a banner per row to draw
/// itself would make listing cost what opening costs. So a World declares a
/// *seed* and this file derives a plate from it locally — no asset, no fetch,
/// and a page that still has a face.
///
/// Everything else in the pane is the same bargain. The tagline, the accent and
/// the route strip are the World's own declarations, carried verbatim. A row
/// with none of them — a Space, or a World this build does not host — draws
/// none of them, rather than the client inventing something to fill the space.
///
/// ## Selecting is not choosing
///
/// Picking a row reads nothing, places nothing and starts nothing — it moves
/// which of the facts already in hand are drawn. Listing is passive; `Open` is
/// the act, and so is jumping to a route, because both place the Orbit.
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

/// The hero plate's height, against the reference's own proportion — its banner
/// takes roughly a third of the window before the action row.
const double kHeroHeight = 168;

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
    final t = context.tokens;
    final rows = ClientScope.watch(context).library_;

    if (rows == null) {
      // Loading is not empty. A surface that drew the empty sentence while the
      // first read was still in flight would be claiming a fact it does not
      // have.
      return Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const Skeleton(width: kRailWidth, height: 28),
          t.gap.x(Space.xl5),
          const Expanded(child: Skeleton(height: kHeroHeight)),
        ],
      );
    }
    if (rows.isEmpty) return const _Empty();

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
        t.box.width(
          // reason: the rail is sized to hold a Space's name at the body size
          // without wrapping, which is a measurement of the type rather than a
          // rung on the spatial scale.
          TokenEscape.rawSize(kRailWidth),
          child: _Rail(
            rows: rows,
            showing: showing,
            onSelect: (row) => setState(() => _selected = row.key),
          ),
        ),
        t.gap.x(Space.xl5),
        Expanded(child: _Detail(showing: showing)),
      ],
    );
  }
}

class _Empty extends StatelessWidget {
  const _Empty();

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text('Library', style: context.titleStyle),
        t.gap.y(Space.xl5),
        // Only sayable because the read succeeded and answered nothing.
        Text('This device serves no Worlds yet.', style: context.bodyStyle),
        t.gap.y(Space.xs),
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
  const _Rail({
    required this.rows,
    required this.showing,
    required this.onSelect,
  });

  final List<LibraryRow> rows;
  final LibraryRow showing;
  final ValueChanged<LibraryRow> onSelect;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: t.padding.only(left: Space.md, bottom: Space.md),
          child: Text('LIBRARY', style: context.factLabelStyle),
        ),
        Expanded(
          child: ListView.separated(
            itemCount: rows.length,
            separatorBuilder: (_, __) => t.gap.y(Space.xxs),
            itemBuilder: (context, index) {
              final row = rows[index];
              return ListTile(
                selected: row.key == showing.key,
                onTap: () => onSelect(row),
                // The mark a World declared, or the plate colour standing in
                // for one. A row is a *thing* rather than a word, which is the
                // reference's own device and the reason its rail scans.
                leading: _Mark(row: row, size: 18),
                title: Text(
                  _name(row),
                  overflow: TextOverflow.ellipsis,
                  // Dimmed when the Orbit is not up — the reference's own way
                  // of saying "you have this but it is not installed", and it
                  // costs no colour the theme did not already answer.
                  style: switch (row.placement) {
                    PlacementView.placed => context.bodyStyle,
                    PlacementView.vacant => context.proseStyle,
                    PlacementView.unknown => context.bodyStyle
                        .copyWith(color: context.status.warning.l800),
                  },
                ),
              );
            },
          ),
        ),
      ],
    );
  }
}

/// A World's mark: the glyph it declared, on the colour it declared.
///
/// Both are optional and neither is invented. With no declaration this is a
/// plain neutral tile — which is what a Space row deserves, because no World
/// has said anything about it.
class _Mark extends StatelessWidget {
  const _Mark({required this.row, required this.size});

  final LibraryRow row;
  final double size;

  @override
  Widget build(BuildContext context) {
    return Container(
      width: size,
      height: size,
      alignment: Alignment.center,
      decoration: BoxDecoration(
        color: _accent(context, row),
        // reason: the mark's corner is a proportion of the mark, not a rung —
        // it is drawn at several sizes and has to keep the same silhouette at
        // each, which a fixed radius from the scale would not.
        borderRadius: TokenEscape.rawRadius(all: size / 4),
      ),
      child: row.worldMount.isEmpty
          ? null
          : Text(
              row.worldMount.substring(0, 1).toUpperCase(),
              style: context.labelStyle.copyWith(
                color: context.surface.l50,
                fontWeight: FontWeight.w700,
                fontSize: size * 0.55,
              ),
            ),
    );
  }
}

class _Detail extends StatelessWidget {
  const _Detail({required this.showing});

  final LibraryRow showing;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return ListView(
      children: [
        _Hero(showing: showing),
        t.gap.y(Space.xl3),
        _ActionRow(showing: showing),
        if (showing.routes.isNotEmpty) ...[
          t.gap.y(Space.xl5),
          _Routes(showing: showing),
        ],
        t.gap.y(Space.xl5),
        _Served(showing: showing),
      ],
    );
  }
}

/// The plate, derived rather than fetched.
class _Hero extends StatelessWidget {
  const _Hero({required this.showing});

  final LibraryRow showing;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final seed = _accent(context, showing);
    return Container(
      height: kHeroHeight,
      padding: t.padding.all(Space.xl5),
      alignment: Alignment.bottomLeft,
      decoration: BoxDecoration(
        borderRadius: t.radius.all(Space.lg),
        // A gradient off the one declared number. Two stops from the same seed
        // rather than a second colour nobody declared — the plate is derived,
        // and derived is all it claims to be.
        gradient: LinearGradient(
          begin: Alignment.topLeft,
          end: Alignment.bottomRight,
          colors: [seed, Color.lerp(seed, context.surface.l900, 0.55)!],
        ),
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            _name(showing),
            style: context.titleStyle.copyWith(
              color: context.surface.l50,
              fontSize: context.font.xl2,
              fontWeight: FontWeight.w700,
            ),
          ),
          if (showing.tagline != null) ...[
            t.gap.y(Space.xs),
            // The World's own line. Absent when it has not said one.
            Text(
              showing.tagline!,
              style: context.bodyStyle.copyWith(color: context.surface.l100),
            ),
          ],
        ],
      ),
    );
  }
}

/// The one act on this page, and the facts beside it — the reference's own
/// arrangement, where the button and the stats share a line.
class _ActionRow extends StatelessWidget {
  const _ActionRow({required this.showing});

  final LibraryRow showing;

  @override
  Widget build(BuildContext context) {
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    final entryPath = showing.opensAt;
    final opening = entryPath != null &&
        view.inFlight.contains(ActionKeys.open(showing.orbit, entryPath));

    return Wrap(
      spacing: 32,
      runSpacing: 16,
      crossAxisAlignment: WrapCrossAlignment.center,
      children: [
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
          // A row that cannot be opened says which kind of cannot. `/` is not a
          // guess to make on a World's behalf, and a Space that is not running
          // is a different case entirely — it opens at its Orbit's own front
          // door, which is what places it.
          tooltip: switch (showing.unopenable) {
            Unopenable.unhosted =>
              'This build hosts no head for that World, so there is nothing '
                  'to open.',
            Unopenable.undeclared =>
              'This World has not declared where to open it.',
            null => 'Hand this World to your browser',
          },
        ),
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

/// The places the World declared, as a strip.
///
/// The reference's tab row, and the same meaning: somewhere inside the thing,
/// one click away. Each is an `Open` at a path the World named — so pressing
/// one places the Orbit exactly as `Open` does, which is why they are drawn as
/// controls rather than as links.
class _Routes extends StatelessWidget {
  const _Routes({required this.showing});

  final LibraryRow showing;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text('GO STRAIGHT TO', style: context.factLabelStyle),
        t.gap.y(Space.sm),
        Wrap(
          spacing: 6,
          runSpacing: 6,
          children: [
            for (final route in showing.routes)
              Button(
                onPressed: view.inFlight
                        .contains(ActionKeys.open(showing.orbit, route.path))
                    ? null
                    : () => client.dispatch(
                          ActionRequest.open(
                            orbit: showing.orbit,
                            entryPath: route.path,
                          ),
                        ),
                label: route.label,
                size: ButtonSize.sm,
                variant: ButtonVariant.secondary,
                tooltip: route.path,
              ),
          ],
        ),
      ],
    );
  }
}

class _Served extends StatelessWidget {
  const _Served({required this.showing});

  final LibraryRow showing;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    // A browser head is bound to an identity and serves every Orbit that
    // identity has; an MCP head is authored against one.
    final serving = ClientScope.watch(context)
        .heads
        .where((head) => head.orbit == null || head.orbit == showing.orbit)
        .map((head) => head.origin)
        .nonNulls
        .toList();
    if (serving.isEmpty) return const SizedBox.shrink();

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text('SERVED BY', style: context.factLabelStyle),
        t.gap.y(Space.xs),
        // Where, and not how to get in: a head's URL carries a run credential
        // and the front page has no use for one. `Open` mints a single-use
        // ticket of its own, which is what that ceremony is for.
        for (final origin in serving) Text(origin, style: context.monoStyle),
      ],
    );
  }
}

class _Fact extends StatelessWidget {
  const _Fact({
    required this.label,
    required this.value,
    this.tone,
    this.mono = false,
  });

  final String label;
  final String value;
  final Color? tone;
  final bool mono;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final style = mono ? context.monoStyle : context.bodyStyle;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(label, style: context.factLabelStyle),
        t.gap.y(Space.xxs),
        Text(value, style: tone == null ? style : style.copyWith(color: tone)),
      ],
    );
  }
}

/// The colour a row is drawn from.
///
/// The World's declared seed when it has one. When it has not — a Space row, or
/// a World this build does not host — a neutral from the theme rather than a
/// colour picked here: an accent nobody declared would be this client claiming
/// a brand on somebody else's behalf.
Color _accent(BuildContext context, LibraryRow row) => row.accent == null
    ? context.surface.l400
    // reason: this colour is the World's own declaration, carried verbatim. It
    // is data arriving from another program, not a choice this client makes, so
    // there is no palette token that could stand for it — snapping it to one
    // would be the client overruling a brand it does not own.
    : TokenEscape.rawColor(0xFF000000 | row.accent!.toInt());

/// A row nobody named is drawn as unnamed. Substituting the id would put
/// something in the name column that is not a name, and a person cannot tell
/// that apart from a World genuinely called that.
String _name(LibraryRow row) => row.displayName ?? 'Unnamed Space';

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
