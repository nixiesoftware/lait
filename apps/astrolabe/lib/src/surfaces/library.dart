/// The front page: a passive Library rail and one selected World.
///
/// One row per installed World — the install list, compiled into the binary.
/// Which Spaces serve a World is the destination's fact: the head's own front
/// page carries the Space selector, and this surface deliberately does not
/// pre-ask it. Steam supplies the durable library spine; GOG supplies the
/// selected-item hierarchy. Astrolabe keeps both honest: selection only
/// changes what is drawn, while `Open` is the act that starts the head and
/// hands it to the browser.
library;

// `Image` is hidden rather than prefixed: covalence's is a network component
// over a `String src`, and every image on this surface is bytes the binary was
// compiled with. There is nothing here for it to fetch.
import 'package:covalence/covalence.dart' hide Surface, Image;
import 'package:covalence/covalence.dart' as cv show Surface;
import 'package:lit_ui/lit_ui.dart' show LightTheme, Lit;
import 'package:covalence/listtile.dart';
import 'package:flutter/material.dart' show Theme;
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import '../settings/window.dart';
import '../shell/face.dart';
import '../shell/lighting.dart';
import '../shell/person.dart';
import '../shell/type.dart';

const double kRailWidth = 224;
const double kHeroHeight = 196;

/// The glance card's column width — the reference client's friends-panel
/// proportion, sized so a face, a name, the AI mark and a presence word fit
/// without crowding each other.
const double kGlanceWidth = 300;
const double _worldActionGlyphSize = 20;

/// The two action coats — launch and stop — as exact inverses of each other.
///
/// Raw rather than theme rungs, and necessarily: a slab that must look the
/// same in either theme cannot take its fill from a ramp that mirrors with
/// polarity — `success.l800` is a step *darker* than `l850` in the light
/// theme and a step *lighter* in the dark one — and its ink cannot come from
/// a text rung that would go pale in the dark and put white on white.
final Color kLaunchSlabFill = TokenEscape.rawColor(0xFF22C55E);
final Color kLaunchSlabInk = TokenEscape.rawColor(0xFFFFFFFF);
final Color kStopSlabFill = TokenEscape.rawColor(0xFFFFFFFF);
final Color kStopSlabInk = TokenEscape.rawColor(0xFF10151A);

/// The corner every action slab is cut with — small enough to read as a
/// square-ish button, large enough to not look like a rendering accident.
const SizeStep kSlabCorner = Space.xs;

class LibrarySurface extends StatefulWidget {
  const LibrarySurface({super.key});

  @override
  State<LibrarySurface> createState() => _LibrarySurfaceState();
}

class _LibrarySurfaceState extends State<LibrarySurface> {
  String? _selected;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final view = ClientScope.watch(context);
    final rows = view.library_;

    if (rows == null) return const _Loading();
    if (rows.isEmpty) return const _Empty();

    // Every installed World is drawn, always. The rail is the install list a
    // build compiles in — a handful of rows, never a corpus — so there is
    // nothing here a search would find that the eye does not.
    final showing = rows.firstWhere(
      (row) => row.key == _selected,
      orElse: () => rows.first,
    );

    return Row(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        t.box.width(
          TokenEscape.rawSize(kRailWidth),
          child: _Rail(
            rows: rows,
            showing: showing,
            view: view,
            onSelect: (row) => setState(() => _selected = row.key),
          ),
        ),
        Expanded(child: _Detail(showing: showing)),
      ],
    );
  }
}

class _Loading extends StatelessWidget {
  const _Loading();

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        t.box.width(
          TokenEscape.rawSize(kRailWidth),
          child: Container(
            decoration: BoxDecoration(
              border: t.stroke.edge(right: context.border.l500),
            ),
            padding: t.padding.fromLTRB(
              Space.xl3,
              Space.xl,
              Space.xl3,
              Space.xl3,
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                const Skeleton(height: 24),
                t.gap.y(Space.xl),
                const Skeleton(height: 32),
                t.gap.y(Space.xl3),
                const Skeleton(height: 28),
                t.gap.y(Space.xs),
                const Skeleton(height: 28),
                t.gap.y(Space.xs),
                const Skeleton(height: 28),
              ],
            ),
          ),
        ),
        const Expanded(child: Skeleton(height: kHeroHeight)),
      ],
    );
  }
}

class _Empty extends StatelessWidget {
  const _Empty();

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Padding(
      padding: t.padding.fromLTRB(
        Space.xl3,
        Space.xl,
        Space.xl3,
        Space.xl3,
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text('Library', style: context.titleStyle),
          t.gap.y(Space.xl5),
          Text('This build installs no Worlds.', style: context.bodyStyle),
          t.gap.y(Space.xs),
          Text(
            // The Library is the install list, compiled into this binary. An
            // empty one is a build with no client packages — a builder's
            // situation, not a person's, and nothing on this machine can
            // change it.
            'A World ships inside the client. This binary was built without '
            'any, so there is nothing to open.',
            style: context.proseStyle,
          ),
        ],
      ),
    );
  }
}

class _Rail extends StatelessWidget {
  const _Rail({
    required this.rows,
    required this.showing,
    required this.view,
    required this.onSelect,
  });

  final List<LibraryRow> rows;
  final LibraryRow? showing;
  final ClientView view;
  final ValueChanged<LibraryRow> onSelect;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final running = rows
        .where((row) => _opening(view, row) || _serving(view).isNotEmpty)
        .toList();
    final ready = rows
        .where((row) => !running.contains(row) && row.opensAt != null)
        .toList();
    final unavailable = rows
        .where((row) => !running.contains(row) && !ready.contains(row))
        .toList();

    return Container(
      key: const ValueKey('library-rail'),
      decoration: BoxDecoration(
        border: t.stroke.edge(right: context.border.l500),
      ),
      // The rail keeps half the gutter and the rows carry the other half, so
      // a row's selected plate bleeds past the text on both sides while the
      // mark, the group labels and the header all start on the same line. The
      // whole gutter here and a second one inside the tile is what put the
      // rows an indent to the right of the word above them.
      padding: t.padding.fromLTRB(
        Space.md,
        Space.xl,
        Space.md,
        Space.xl3,
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: t.padding.symmetric(h: Space.md),
            child: Row(
              children: [
                Expanded(
                  child: Text(
                    'LIBRARY',
                    style: context.factLabelStyle.copyWith(
                      color: context.text.l900,
                      fontWeight: FontWeight.w700,
                    ),
                  ),
                ),
                Text('${rows.length}', style: context.labelStyle),
              ],
            ),
          ),
          t.gap.y(Space.xl3),
          Expanded(
            child: ListView(
              padding: EdgeInsets.zero,
              children: [
                if (running.isNotEmpty)
                  _RailSection(
                    label: 'RUNNING',
                    rows: running,
                    showing: showing,
                    view: view,
                    onSelect: onSelect,
                  ),
                // Ready carries no heading: it is the ordinary state of an
                // installed World, and a label over it names what every row
                // would say if it said nothing. The two headings that remain
                // are the ones worth reading — a World that is up, and one
                // this build cannot open.
                if (ready.isNotEmpty)
                  _RailSection(
                    rows: ready,
                    showing: showing,
                    view: view,
                    onSelect: onSelect,
                  ),
                if (unavailable.isNotEmpty)
                  _RailSection(
                    label: 'UNAVAILABLE',
                    rows: unavailable,
                    showing: showing,
                    view: view,
                    onSelect: onSelect,
                  ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

class _RailSection extends StatelessWidget {
  const _RailSection({
    this.label,
    required this.rows,
    required this.showing,
    required this.view,
    required this.onSelect,
  });

  /// The heading over the group, or null for a group that needs none — the
  /// rows then start flush against the section above.
  final String? label;
  final List<LibraryRow> rows;
  final LibraryRow? showing;
  final ClientView view;
  final ValueChanged<LibraryRow> onSelect;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Padding(
      padding: t.padding.only(bottom: Space.xl3),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (label != null)
            Padding(
              padding: t.padding.only(left: Space.md, bottom: Space.sm),
              child: Text(label!, style: context.factLabelStyle),
            ),
          for (final row in rows)
            Padding(
              padding: t.padding.only(bottom: Space.xxs),
              child: ListTile(
                variant: ListTileVariant.dense,
                // The rail's other half. Dense defaults to a full gutter,
                // which on top of the rail's own put every row an indent
                // right of the header it sits under.
                contentPadding: t.padding.symmetric(h: Space.md, v: Space.xs),
                selected: row.key == showing?.key,
                onTap: () => onSelect(row),
                // 24 rather than 20: a World's own mark has detail in it, and
                // four pixels is the difference between a shape and a smudge.
                leading: _Mark(row: row, size: 24),
                title: Text(
                  _name(row),
                  overflow: TextOverflow.ellipsis,
                ),
                tooltip: '${_name(row)} — ${_lifecycleCopy(view, row).label}',
              ),
            ),
        ],
      ),
    );
  }
}

/// A World's mark: its own artwork where it ships one, and a plate cut from
/// its accent where it does not.
///
/// The fallback is not a placeholder. Every World was drawn this way before
/// any of them shipped art, and a World that ships none is making a choice
/// rather than missing a file — so the derived plate stays a first-class
/// answer instead of a grey box waiting for something.
class _Mark extends StatelessWidget {
  const _Mark({required this.row, required this.size});

  final LibraryRow row;
  final double size;

  @override
  Widget build(BuildContext context) {
    final radius = TokenEscape.rawRadius(all: size / 4);
    final mark = ClientScope.of(context).artworkFor(row.worldMount).mark;
    if (mark != null) {
      return ClipRRect(
        borderRadius: radius,
        child: Image.memory(
          mark,
          width: size,
          height: size,
          fit: BoxFit.cover,
          // A mark is drawn an order below the size it ships at, so the
          // filter is doing real work rather than nudging a pixel.
          filterQuality: FilterQuality.medium,
        ),
      );
    }
    return Container(
      width: size,
      height: size,
      alignment: Alignment.center,
      decoration: BoxDecoration(
        color: _accent(context, row),
        borderRadius: radius,
      ),
      child: Text(
        row.worldMount.substring(0, 1).toUpperCase(),
        style: context.labelStyle.copyWith(
          color: cv.Surface.onSolid.resolve(context),
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
      padding: EdgeInsets.zero,
      children: [
        _Hero(showing: showing),
        _ActionPanel(showing: showing),
        Padding(
          padding: t.padding.all(Space.xl5),
          child: Column(
            key: const ValueKey('library-detail-content'),
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              // No route quick-access here, though the template declares
              // them: Board/Issues/Specs are the World's own navigation, and
              // a client that drew them would be reaching across the
              // boundary it exists to keep. Astrolabe surfaces lifecycle;
              // `Open` is the one act, and everything past it is the
              // World's.
              //
              // Nor the World's own facts: the entry path and the address of
              // the head serving it are the destination's to state. The band
              // above acts, the footer's launch notice carries the address it
              // opened, and World settings holds the rest.
              _Glance(showing: showing),
            ],
          ),
        ),
      ],
    );
  }
}

class _Hero extends StatelessWidget {
  const _Hero({required this.showing});

  final LibraryRow showing;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final seed = _accent(context, showing);
    final onHero = cv.Surface.onSolid.resolve(context);

    // The World's own frame, where it ships one. The accent gradient stays
    // either way and goes over the top of it as a scrim: a title in white on
    // bare artwork is legible until the day a World ships a pale one, and
    // which day that is would not be this client's to find out. Translucent
    // where there is art to show through, opaque where there is none — the
    // artless case draws exactly what it drew before.
    final art = ClientScope.of(context).artworkFor(showing.worldMount).hero;
    final wash = Color.lerp(seed, context.surface.l950, 0.62)!;
    final scrim = LinearGradient(
      begin: Alignment.topLeft,
      end: Alignment.bottomRight,
      colors: art == null
          ? [seed, wash]
          : [seed.withValues(alpha: 0.72), wash.withValues(alpha: 0.88)],
    );

    return Container(
      key: const ValueKey('library-hero'),
      height: kHeroHeight,
      decoration: BoxDecoration(
        border: t.stroke.edge(bottom: seed.withValues(alpha: 0.55)),
      ),
      child: Stack(
        fit: StackFit.expand,
        children: [
          if (art != null)
            Image.memory(art, fit: BoxFit.cover, filterQuality: FilterQuality.medium),
          DecoratedBox(decoration: BoxDecoration(gradient: scrim)),
          // The name, and nothing else. What sat around it said what the rest
          // of the surface already says: the eyebrow named the kind of thing
          // every row in this Library is, the badge repeated the state the
          // band below acts on, and the tagline is a list's line — it earns
          // its place where Worlds are being told apart, not on the one the
          // person has already chosen.
          Padding(
            padding: t.padding.all(Space.xl5),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              mainAxisAlignment: MainAxisAlignment.end,
              children: [
                Text(
                  _name(showing),
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                  style: context.titleStyle.copyWith(
                    color: onHero,
                    fontSize: context.font.xl3,
                    fontWeight: FontWeight.w700,
                  ),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

class _ActionPanel extends StatelessWidget {
  const _ActionPanel({required this.showing});

  final LibraryRow showing;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    final entryPath = showing.opensAt;
    final opening = _opening(view, showing);
    final lifecycle = _lifecycleCopy(view, showing);
    // The head is per-identity and serves every installed World, so "running"
    // is the head's own liveness: an owned browser head reporting an address.
    final heads = _serving(view);
    final running = !opening && heads.isNotEmpty;
    final activeOrigin = heads.isEmpty ? null : heads.first.origin;
    // Stopping is offered against an *owned* head and nothing else. A head
    // this client did not start belongs to whoever ran it, and ownership is
    // the boundary the supervisor enforces — a control that pretended
    // otherwise would be a button whose refusal is the only way to learn it
    // was never ours.
    final stoppable = _stoppable(view);
    final stopping =
        stoppable != null && view.inFlight.contains(ActionKeys.stopHead(stoppable.id));

    return Container(
      key: const ValueKey('library-open-band'),
      padding: t.padding.symmetric(h: Space.xl5, v: Space.xl2),
      decoration: BoxDecoration(
        color: context.surface.l100,
        border: t.stroke.edge(bottom: context.border.l500),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.center,
        children: [
          // The band carries the act and nothing else. The version is a fact
          // about the World rather than something to do with it, and the
          // detail column below already answers that kind of question.
          Expanded(
            child: Align(
              alignment: AlignmentDirectional.centerStart,
              child: _WorldAction(
                showing: showing,
                running: running,
                opening: opening,
                stopping: stopping,
                lifecycle: lifecycle,
                onOpen: entryPath == null || opening
                    ? null
                    : () => client.dispatch(
                          ActionRequest.open(entryPath: entryPath),
                        ),
                onStop: stoppable == null || stopping
                    ? null
                    : () => client.dispatch(
                          ActionRequest.stopHead(id: stoppable.id),
                        ),
              ),
            ),
          ),
          t.gap.x(Space.md),
          Button(
            onPressed: () => WorldSettingsScope.open(
              context,
              WorldSettingsSnapshot(
                key: showing.key,
                name: _name(showing),
                worldMount: showing.worldMount,
                entryPath: showing.opensAt,
                version: showing.version,
                activeOrigin: activeOrigin,
                dark: Theme.of(context).brightness == Brightness.dark,
              ),
            ),
            icon: AppIcons.settings,
            semanticLabel: '${_name(showing)} settings',
            tooltip: 'World settings',
            variant: ButtonVariant.ghost,
            size: ButtonSize.iconSm,
          ),
        ],
      ),
    );
  }
}

class _WorldAction extends StatelessWidget {
  const _WorldAction({
    required this.showing,
    required this.running,
    required this.opening,
    required this.stopping,
    required this.lifecycle,
    required this.onOpen,
    required this.onStop,
  });

  final LibraryRow showing;
  final bool running;
  final bool opening;
  final bool stopping;
  final ({
    String label,
    String description,
    BadgeVariant variant,
    BadgeDotTone dot,
  }) lifecycle;
  final VoidCallback? onOpen;
  final VoidCallback? onStop;

  @override
  Widget build(BuildContext context) {
    if (stopping) return const _PendingSlab(label: 'STOPPING');

    if (running) {
      return _RunningControl(
        onStop: onStop,
        onOpen: onOpen,
        openTooltip: _openTooltip(showing, running: true),
      );
    }

    if (opening) return const _PendingSlab(label: 'LAUNCHING');

    if (onOpen != null) {
      return _LaunchControl(
        onOpen: onOpen!,
        tooltip: _openTooltip(showing, running: false),
      );
    }

    return _LifecycleState(
      label: lifecycle.label,
      icon: AppIcons.info,
      tone: context.text.l700,
    );
  }
}

/// The launch control — the reference client's solid green slab.
///
/// Green is launch's alone. The colour that starts a World never sits under
/// the control that stops one, so the two acts read apart before either
/// label is.
///
/// The sheen is derived, not hand-tuned: a [Lit] surface under one
/// top-mounted directional light computes the gradient the reference
/// client's button wears, and retunes itself if the fill ever changes.
class _LaunchControl extends StatefulWidget {
  const _LaunchControl({required this.onOpen, required this.tooltip});

  final VoidCallback onOpen;
  final String tooltip;

  @override
  State<_LaunchControl> createState() => _LaunchControlState();
}

class _LaunchControlState extends State<_LaunchControl> {
  double _hover = 0;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final fill = kLaunchSlabFill;
    final ink = kLaunchSlabInk;
    // The ambient scene first — what the lighting workbench edits is what
    // this control wears — and the canonical scene only where no window
    // mounted one (a bare test harness).
    final scene = LightTheme.maybeOf(context) ?? kAstrolabeScene;
    return Semantics(
      button: true,
      label: 'Launch World',
      child: Tooltip(
        message: widget.tooltip,
        child: Lit(
          scene: scene,
          baseColor: Color.lerp(fill, ink, _hover * 0.12)!,
          curvature: 0.12,
          elevation: 3,
          borderRadius: t.radius.all(kSlabCorner),
          onTap: widget.onOpen,
          onHoverChange: (value) => setState(() => _hover = value),
          // No `alignment` here, and that is load-bearing: a Container given
          // one expands to its constraints instead of its child, and this slab
          // sits directly in a Wrap that offers the whole band. The split
          // control escapes it by being a min-width Row first; this one has no
          // Row above it, so alignment would make LAUNCH band-wide.
          child: Container(
            height: 40,
            padding: t.padding.symmetric(h: Space.xl3),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(
                  AppIcons.playArrow,
                  size: _worldActionGlyphSize,
                  color: ink,
                ),
                t.gap.x(Space.sm),
                Text(
                  'LAUNCH',
                  style: context.bodyStyle.copyWith(
                    color: ink,
                    fontSize: _worldActionGlyphSize,
                    fontWeight: FontWeight.w400,
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// The running control — the reference client's solid split button in its
/// stop coat: one white slab, the stop mark and STOP on its left, the browser
/// handoff on the right of a hairline. White, never green — green is launch's
/// alone, and a stop control wearing the colour that starts is how a slip
/// lands on the wrong act.
///
/// Unlike the reference's split, the halves are different acts: STOP ends the
/// head, and the handoff goes to it while it still serves. Each half answers
/// the pointer alone, brightening under it while its sibling rests.
///
/// What is stopped is the **head**, which serves every installed World — so
/// the word on the tooltip says that, rather than letting a control on one
/// World's row imply it stops only that one. When no owned head is serving,
/// [onStop] is null: the left half falls away rather than offering an act
/// this client has no standing to perform.
///
/// The sheen is derived, not hand-tuned: [Lit] surfaces under one
/// top-mounted directional light compute the gradient the reference
/// client's button wears, and retune themselves if the fill ever changes.
class _RunningControl extends StatefulWidget {
  const _RunningControl({
    required this.onStop,
    required this.onOpen,
    required this.openTooltip,
  });

  final VoidCallback? onStop;
  final VoidCallback? onOpen;
  final String openTooltip;

  @override
  State<_RunningControl> createState() => _RunningControlState();
}

class _RunningControlState extends State<_RunningControl> {
  double _hoverStop = 0;
  double _hoverHandoff = 0;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final fill = kStopSlabFill;
    final ink = kStopSlabInk;
    // The hovered segment lifts toward its own ink; Lit hands back the
    // animated 0..1 and imposes no look of its own. On a white slab that
    // reads as settling rather than brightening, which is the same gesture
    // the vivid fills make — toward the ink, whichever way that runs.
    Color raised(double hover) => Color.lerp(fill, ink, hover * 0.12)!;
    // The ambient scene first — what the lighting workbench edits is what
    // this control wears — and the canonical scene only where no window
    // mounted one (a bare test harness).
    final scene = LightTheme.maybeOf(context) ?? kAstrolabeScene;
    final stoppable = widget.onStop != null;
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        if (stoppable) ...[
          Semantics(
            button: true,
            label: 'Stop the head',
            child: Tooltip(
              message: 'Stop the head serving your Worlds',
              child: Lit(
                scene: scene,
                baseColor: raised(_hoverStop),
                curvature: 0.12,
                elevation: 3,
                borderRadius: t.radius.corner(
                  topLeft: kSlabCorner,
                  bottomLeft: kSlabCorner,
                ),
                onTap: widget.onStop,
                onHoverChange: (value) => setState(() => _hoverStop = value),
                child: Container(
                  height: 40,
                  padding: t.padding.symmetric(h: Space.xl3),
                  alignment: Alignment.center,
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Icon(
                        AppIcons.close,
                        size: _worldActionGlyphSize,
                        color: ink,
                      ),
                      t.gap.x(Space.sm),
                      Text(
                        'STOP',
                        style: context.bodyStyle.copyWith(
                          color: ink,
                          fontSize: _worldActionGlyphSize,
                          fontWeight: FontWeight.w400,
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            ),
          ),
          Container(
            width: t.stroke.xxs,
            height: 40,
            // reason: the seam splitting the slab is a shade of its own ink,
            // not a theme rung — a grey line on white in any theme. Lighter
            // than the vivid fills wanted, because full 25% black on white is
            // a rule through the middle of a button rather than a seam.
            color: TokenEscape.rawColor(0x24000000),
          ),
        ],
        Semantics(
          button: true,
          label: 'Go to running World',
          child: Tooltip(
            message: widget.openTooltip,
            child: Lit(
              scene: scene,
              baseColor: raised(_hoverHandoff),
              curvature: 0.12,
              elevation: 3,
              borderRadius: stoppable
                  ? t.radius.corner(
                      topRight: kSlabCorner,
                      bottomRight: kSlabCorner,
                    )
                  : t.radius.all(kSlabCorner),
              onTap: widget.onOpen,
              onHoverChange: (value) => setState(() => _hoverHandoff = value),
              child: Container(
                height: 40,
                // An unsplit slab is the whole control, so it carries the
                // word too — a lone glyph would be a button with no name.
                padding: t.padding.symmetric(
                  h: stoppable ? Space.md : Space.xl3,
                ),
                alignment: Alignment.center,
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    if (!stoppable) ...[
                      Text(
                        'OPEN',
                        style: context.bodyStyle.copyWith(
                          color: ink,
                          fontSize: _worldActionGlyphSize,
                          fontWeight: FontWeight.w400,
                        ),
                      ),
                      t.gap.x(Space.sm),
                    ],
                    Icon(
                      AppIcons.openInNew,
                      size: _worldActionGlyphSize,
                      color: ink,
                    ),
                  ],
                ),
              ),
            ),
          ),
        ),
      ],
    );
  }
}

/// An act in flight — LAUNCHING, STOPPING — wearing the running control's own
/// coat.
///
/// The same slab as STOP, deliberately: these states occupy the action's slot
/// for a second or two, and a translucent pill in that slot made the band
/// change weight and shape under a transition. Launch hands off to this, and
/// this hands off to stop, without the row moving underneath the pointer.
///
/// Not interactive, and `Lit` handles that on its own: with a null `onTap` it
/// draws the surface and lets pointer events pass straight through.
class _PendingSlab extends StatelessWidget {
  const _PendingSlab({required this.label});

  final String label;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final scene = LightTheme.maybeOf(context) ?? kAstrolabeScene;
    return Semantics(
      label: label,
      liveRegion: true,
      child: Lit(
        scene: scene,
        baseColor: kStopSlabFill,
        curvature: 0.12,
        elevation: 3,
        borderRadius: t.radius.all(kSlabCorner),
        // No `alignment` on the Container below, for the reason the launch
        // slab carries in full: one would expand this to the whole band.
        child: Container(
          height: 40,
          padding: t.padding.symmetric(h: Space.xl3),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Progress.spinner(size: ProgressSize.lg, color: kStopSlabInk),
              t.gap.x(Space.sm),
              Text(
                label,
                style: context.bodyStyle.copyWith(
                  color: kStopSlabInk,
                  fontSize: _worldActionGlyphSize,
                  fontWeight: FontWeight.w400,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// The row's state where there is no act to offer — a World this build cannot
/// open. A readout, not a control, and drawn as one.
class _LifecycleState extends StatelessWidget {
  const _LifecycleState({
    required this.label,
    required this.tone,
    this.icon,
  });

  final String label;
  final Color tone;
  final IconData? icon;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Semantics(
      label: label,
      child: Container(
        height: 40,
        constraints: const BoxConstraints(minWidth: 124),
        padding: t.padding.symmetric(h: Space.xl3),
        decoration: BoxDecoration(
          color: tone.withValues(alpha: 0.10),
          border: Border.all(color: tone.withValues(alpha: 0.45)),
          borderRadius: t.radius.all(Space.sm),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            if (icon != null) Icon(icon, size: 16, color: tone),
            t.gap.x(Space.sm),
            Text(
              label,
              style: context.bodyStyle.copyWith(
                color: tone,
                fontWeight: FontWeight.w700,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _Glance extends StatelessWidget {
  const _Glance({required this.showing});

  final LibraryRow showing;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;

    // No head of its own: the tiers inside already say who is in the World
    // and who merely holds it, and a title above them would count what the
    // two lines beneath it count better.
    final glance = _InfoPanel(child: _People(people: showing.people));

    // It was the reference client's right-hand sidebar while operational
    // panels flowed beside it; with those gone it flanks nothing, so it
    // starts at the page's own margin instead of floating against the far
    // edge. Kept at its designed width rather than stretched — the card is a
    // column of people, and a column of people 700 wide is a table.
    return LayoutBuilder(
      builder: (context, constraints) {
        if (constraints.maxWidth < 620) return glance;
        return Align(
          alignment: AlignmentDirectional.topStart,
          child: t.box.width(TokenEscape.rawSize(kGlanceWidth), child: glance),
        );
      },
    );
  }
}

/// The book ∩ this Space, at a glance — the closest honest analogue of the
/// reference client's "friends who play" panel. Every row resolves through
/// the book (the face, the name, the AI mark) with presence measured in
/// this Space alone, and the two absences stay apart: an unread book and a
/// Space nobody in the book is addressed in say different things. The
/// authoritative roster is not this panel — reading it is the act of
/// choosing the Space on Members, and a glance never places anything.
class _People extends StatelessWidget {
  const _People({required this.people});

  final List<WorldPersonRow>? people;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final rows = people;
    if (rows == null) {
      return Text('The book has not been read.', style: context.proseStyle);
    }
    if (rows.isEmpty) {
      return Text(
        'Nobody in the book is addressed here.',
        style: context.proseStyle,
      );
    }
    // Two tiers of liveness, the reference client's shape: full canonical
    // rows for people IN the World right now — a launched World is the
    // nearest presence there is — and bare faces for everyone else the book
    // addresses here, ordered by reach with the measured absence last.
    final here = rows.where((person) => person.here).toList();
    final holding = [
      ...rows.where(
          (person) => !person.here && person.presence == PresenceView.online),
      ...rows.where(
          (person) => !person.here && person.presence == PresenceView.away),
      ...rows.where((person) => !person.here && person.presence == null),
      ...rows.where(
          (person) => !person.here && person.presence == PresenceView.offline),
    ];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (here.isNotEmpty) ...[
          Text(
            here.length == 1
                ? '1 is in the World now'
                : '${here.length} are in the World now',
            style: context.labelStyle.copyWith(
              color: context.text.l800,
              fontWeight: FontWeight.w600,
            ),
          ),
          t.gap.y(Space.md),
          for (final person in here) ...[
            PersonTile(
              name: person.name,
              picture: person.picture,
              presence: person.presence,
              agent: person.agent,
              size: 32,
            ),
            t.gap.y(Space.md),
          ],
        ],
        if (here.isNotEmpty && holding.isNotEmpty) t.gap.y(Space.md),
        if (holding.isNotEmpty) ...[
          Text(
            holding.length == 1
                ? '1 has it in their library'
                : '${holding.length} have it in their library',
            style: context.labelStyle.copyWith(
              color: context.text.l800,
              fontWeight: FontWeight.w600,
            ),
          ),
          t.gap.y(Space.md),
          // Faces only, the reference client's "played previously" grid —
          // the name travels as a tooltip, and liveness still reads through
          // the same grey-out the canonical tile wears.
          Wrap(
            spacing: t.size.xs,
            runSpacing: t.size.xs,
            children: [
              for (final person in holding)
                Tooltip(
                  message: presenceLabel(person.presence) == null
                      ? person.name
                      : '${person.name} — ${presenceLabel(person.presence)}',
                  // The label a pointer earns by hovering, a screen reader
                  // gets outright — a face with no announced name would be
                  // a person a reader cannot tell apart from the next. The
                  // plate's own contents are excluded: a monogram letter is
                  // decoration, not a second thing to announce.
                  child: Semantics(
                    container: true,
                    excludeSemantics: true,
                    label: presenceLabel(person.presence) == null
                        ? person.name
                        : '${person.name} — ${presenceLabel(person.presence)}',
                    child: Opacity(
                      opacity: person.presence == PresenceView.offline
                          ? 0.45
                          : (person.presence == PresenceView.away ? 0.7 : 1.0),
                      child: FacePlate(
                        picture: person.picture,
                        name: person.name,
                        size: 28,
                      ),
                    ),
                  ),
                ),
            ],
          ),
        ],
      ],
    );
  }
}

class _InfoPanel extends StatelessWidget {
  const _InfoPanel({required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Container(
      padding: t.padding.all(Space.xl3),
      decoration: BoxDecoration(
        // One layer above the page, not level with it: a card that shares
        // the background's color is a border pretending to be a surface.
        color: context.surface.l100,
        border: Border.all(color: context.border.l500, width: t.stroke.xxs),
        borderRadius: t.radius.all(Space.md),
      ),
      child: child,
    );
  }
}

/// The heads serving this identity — the browser heads bound to no one
/// Orbit, because one identity head serves every installed World. What they
/// answer is the head's own liveness, which is what "running" means on a
/// World row: the destination is up and `Open` is a handoff, not a start.
List<HeadRow> _serving(ClientView view) =>
    view.heads.where((head) => head.orbit == null).toList();

/// The serving head this client may stop, or `None` when there is none it
/// owns.
///
/// Ownership is the boundary, not a preference: the supervisor stops what it
/// started, and a head somebody ran from a terminal is theirs. Answering with
/// the head itself rather than a bool is what lets the caller name the one it
/// is stopping — `head.stop:<id>` is per-head, so a second head's stop does
/// not disable this one's control.
HeadRow? _stoppable(ClientView view) {
  for (final head in _serving(view)) {
    if (head.owned) return head;
  }
  return null;
}

enum _Lifecycle { opening, running, ready, unavailable }

({
  String label,
  String description,
  BadgeVariant variant,
  BadgeDotTone dot,
}) _lifecycleCopy(ClientView view, LibraryRow row) {
  final lifecycle = _lifecycle(view, row);
  return switch (lifecycle) {
    _Lifecycle.opening => (
        label: 'Launching',
        description: 'Starting this World\'s head and preparing the handoff.',
        variant: BadgeVariant.solid,
        dot: BadgeDotTone.brand,
      ),
    _Lifecycle.running => (
        label: 'Running',
        description: 'This World\'s head is up and ready to view.',
        variant: BadgeVariant.success,
        dot: BadgeDotTone.success,
      ),
    _Lifecycle.ready => (
        label: 'Ready',
        description: 'Launch starts the World and hands it to your browser.',
        variant: BadgeVariant.outline,
        dot: BadgeDotTone.neutral,
      ),
    _Lifecycle.unavailable => (
        label: 'Unavailable',
        description: 'This World has not declared an entry path.',
        variant: BadgeVariant.muted,
        dot: BadgeDotTone.neutral,
      ),
  };
}

_Lifecycle _lifecycle(ClientView view, LibraryRow row) {
  // A browser handoff to a head already up never changes the World-level
  // state. Stopping the head does — and until the re-read confirms it, the
  // honest state is still Running.
  if (_opening(view, row)) return _Lifecycle.opening;
  if (row.opensAt == null) return _Lifecycle.unavailable;
  if (_serving(view).isNotEmpty) return _Lifecycle.running;
  return _Lifecycle.ready;
}

bool _opening(ClientView view, LibraryRow row) {
  final path = row.opensAt;
  return path != null && view.inFlight.contains(ActionKeys.open(path));
}

String _openTooltip(LibraryRow row, {required bool running}) {
  if (row.opensAt == null) {
    return 'This World has not declared where to open it.';
  }
  return running
      ? 'Take me to the running World'
      : 'Start this World and hand it to my browser';
}

Color _accent(BuildContext context, LibraryRow row) => row.accent == null
    ? context.surface.l500
    // reason: the World owns this seed. Snapping it to Astrolabe's brand ramp
    // would replace a declaration with the client's opinion.
    : TokenEscape.rawColor(0xFF000000 | row.accent!.toInt());

String _name(LibraryRow row) => row.displayName;
