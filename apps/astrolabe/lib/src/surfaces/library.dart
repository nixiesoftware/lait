/// The front page: a passive Library rail and one selected World.
///
/// Steam supplies the durable library spine; GOG supplies the selected-item
/// hierarchy. Astrolabe keeps both honest: selection only changes what is
/// drawn, while `Open` or a World-declared route is the act that can place an
/// Orbit and hand it to the browser.
library;

import 'package:covalence/covalence.dart' hide Surface;
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

class LibrarySurface extends StatefulWidget {
  const LibrarySurface({super.key});

  @override
  State<LibrarySurface> createState() => _LibrarySurfaceState();
}

class _LibrarySurfaceState extends State<LibrarySurface> {
  String? _selected;
  String _query = '';

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final view = ClientScope.watch(context);
    final rows = view.library_;

    if (rows == null) return const _Loading();
    if (rows.isEmpty) return const _Empty();

    final needle = _query.trim().toLowerCase();
    final visible = needle.isEmpty
        ? rows
        : rows
            .where((row) =>
                _name(row).toLowerCase().contains(needle) ||
                row.space.toLowerCase().contains(needle) ||
                row.worldMount.toLowerCase().contains(needle))
            .toList();

    final showing = visible.isEmpty
        ? null
        : visible.firstWhere(
            (row) => row.key == _selected,
            orElse: () => visible.first,
          );

    return Row(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        t.box.width(
          TokenEscape.rawSize(kRailWidth),
          child: _Rail(
            rows: visible,
            allCount: rows.length,
            showing: showing,
            view: view,
            onQuery: (query) => setState(() => _query = query),
            onSelect: (row) => setState(() => _selected = row.key),
          ),
        ),
        Expanded(
          child:
              showing == null ? const _NoMatches() : _Detail(showing: showing),
        ),
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
          Text('This device serves no Worlds yet.', style: context.bodyStyle),
          t.gap.y(Space.xs),
          Text(
            // Founding and entering are not this client's flows yet; saying
            // where they live beats pointing at a tab that no longer exists.
            'Found a Space, or enter one from an invite, from a World '
            'head\'s Welcome page — this client draws what the daemon '
            'already serves.',
            style: context.proseStyle,
          ),
        ],
      ),
    );
  }
}

class _NoMatches extends StatelessWidget {
  const _NoMatches();

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Center(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(AppIcons.search, color: context.text.l700),
          t.gap.y(Space.md),
          Text('No Worlds match this search.', style: context.bodyStyle),
        ],
      ),
    );
  }
}

class _Rail extends StatelessWidget {
  const _Rail({
    required this.rows,
    required this.allCount,
    required this.showing,
    required this.view,
    required this.onQuery,
    required this.onSelect,
  });

  final List<LibraryRow> rows;
  final int allCount;
  final LibraryRow? showing;
  final ClientView view;
  final ValueChanged<String> onQuery;
  final ValueChanged<LibraryRow> onSelect;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final running = rows
        .where((row) =>
            _opening(view, row) || row.placement == PlacementView.placed)
        .toList();
    final ready = rows
        .where((row) =>
            !_opening(view, row) &&
            row.placement == PlacementView.vacant &&
            row.unopenable == null)
        .toList();
    final unavailable = rows
        .where((row) => !running.contains(row) && !ready.contains(row))
        .toList();

    return Container(
      key: const ValueKey('library-rail'),
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
          Row(
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
              Text('$allCount', style: context.labelStyle),
            ],
          ),
          t.gap.y(Space.md),
          Input(
            hint: 'Search Worlds',
            semanticLabel: 'Search Library',
            size: InputSize.sm,
            search: true,
            onChanged: onQuery,
          ),
          t.gap.y(Space.xl3),
          Expanded(
            child: rows.isEmpty
                ? Text('No matches', style: context.labelStyle)
                : ListView(
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
                      if (ready.isNotEmpty)
                        _RailSection(
                          label: 'READY',
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
    required this.label,
    required this.rows,
    required this.showing,
    required this.view,
    required this.onSelect,
  });

  final String label;
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
          Padding(
            padding: t.padding.only(left: Space.md, bottom: Space.sm),
            child: Text(label, style: context.factLabelStyle),
          ),
          for (final row in rows)
            Padding(
              padding: t.padding.only(bottom: Space.xxs),
              child: ListTile(
                variant: ListTileVariant.dense,
                selected: row.key == showing?.key,
                onTap: () => onSelect(row),
                leading: _Mark(row: row, size: 20),
                title: Text(
                  _name(row),
                  overflow: TextOverflow.ellipsis,
                  style: row.placement == PlacementView.unknown
                      ? context.bodyStyle.copyWith(
                          color: context.status.warning.l800,
                        )
                      : null,
                ),
                tooltip: '${_name(row)} — ${_lifecycleCopy(view, row).label}',
              ),
            ),
        ],
      ),
    );
  }
}

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
        borderRadius: TokenEscape.rawRadius(all: size / 4),
      ),
      child: row.worldMount.isEmpty
          ? null
          : Text(
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
              _Details(showing: showing),
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
    final view = ClientScope.watch(context);
    final seed = _accent(context, showing);
    final onHero = cv.Surface.onSolid.resolve(context);
    final lifecycle = _lifecycleCopy(view, showing);

    return Container(
      key: const ValueKey('library-hero'),
      height: kHeroHeight,
      padding: t.padding.all(Space.xl5),
      decoration: BoxDecoration(
        border: t.stroke.edge(bottom: seed.withValues(alpha: 0.55)),
        gradient: LinearGradient(
          begin: Alignment.topLeft,
          end: Alignment.bottomRight,
          colors: [seed, Color.lerp(seed, context.surface.l950, 0.62)!],
        ),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Text(
                showing.worldMount.isEmpty ? 'SPACE' : 'WORLD',
                style: context.factLabelStyle.copyWith(
                  color: onHero,
                  fontWeight: FontWeight.w700,
                ),
              ),
              const Spacer(),
              Badge(
                label: lifecycle.label,
                color: onHero,
                radius: Space.xs,
              ),
            ],
          ),
          const Spacer(),
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
          if (showing.tagline != null) ...[
            t.gap.y(Space.xs),
            Text(
              showing.tagline!,
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
              style: context.bodyStyle.copyWith(color: onHero),
            ),
          ],
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
    final running = showing.placement == PlacementView.placed;
    final lifecycle = _lifecycleCopy(view, showing);
    final sync = _syncCopy(context, showing);
    final heads = _matchingHeads(view, showing);
    final activeOrigin = heads.isEmpty ? null : heads.first.origin;

    return Container(
      key: const ValueKey('library-open-band'),
      padding: t.padding.symmetric(h: Space.xl5, v: Space.xl3),
      decoration: BoxDecoration(
        color: context.surface.l100,
        border: t.stroke.edge(bottom: context.border.l500),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.center,
        children: [
          Expanded(
            child: Wrap(
              spacing: t.size.xl5,
              runSpacing: t.size.xl3,
              crossAxisAlignment: WrapCrossAlignment.center,
              children: [
                _WorldAction(
                  showing: showing,
                  running: running,
                  opening: opening,
                  lifecycle: lifecycle,
                  onOpen: entryPath == null || opening
                      ? null
                      : () => client.dispatch(
                            ActionRequest.open(
                              orbit: showing.orbit,
                              entryPath: entryPath,
                            ),
                          ),
                ),
                _StatusReadout(
                  icon: AppIcons.accessTime,
                  label: 'LAST OPENED',
                  value: _ago(showing.lastOpened),
                ),
                _StatusReadout(
                  icon: AppIcons.inventory2,
                  label: 'VERSION',
                  value: showing.version == null
                      ? 'Not reported'
                      : 'v${showing.version}',
                ),
              ],
            ),
          ),
          t.gap.x(Space.xl3),
          _StatusReadout(
            icon: sync.icon,
            label: 'SYNC STATUS',
            value: sync.label,
            tone: sync.tone,
            tooltip: sync.detail,
          ),
          t.gap.x(Space.md),
          Button(
            onPressed: () => WorldSettingsScope.open(
              context,
              WorldSettingsSnapshot(
                key: showing.key,
                name: _name(showing),
                orbit: showing.orbit,
                syncLabel: sync.label,
                syncDetail: sync.detail,
                store: showing.store,
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
    required this.lifecycle,
    required this.onOpen,
  });

  final LibraryRow showing;
  final bool running;
  final bool opening;
  final ({
    String label,
    String description,
    BadgeVariant variant,
    BadgeDotTone dot,
  }) lifecycle;
  final VoidCallback? onOpen;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;

    if (running) {
      return _RunningControl(
        onOpen: onOpen,
        tooltip: _openTooltip(showing, running: true),
      );
    }

    if (opening) {
      return _LifecycleState(
        label: 'Launching',
        loading: true,
        tone: context.text.l950,
        large: true,
      );
    }

    if (onOpen != null) {
      return Button(
        onPressed: onOpen,
        semanticLabel: 'Launch World',
        variant: ButtonVariant.primary,
        size: ButtonSize.lg,
        borderRadius: t.radius.all(Space.xxs),
        tooltip: _openTooltip(showing, running: false),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(
              AppIcons.playArrow,
              size: _worldActionGlyphSize,
              color: context.surface.l50,
            ),
            t.gap.x(Space.sm),
            Text(
              'LAUNCH',
              style: context.bodyStyle.copyWith(
                color: context.surface.l50,
                fontSize: _worldActionGlyphSize,
                fontWeight: FontWeight.w400,
              ),
            ),
          ],
        ),
      );
    }

    return _LifecycleState(
      label: lifecycle.label,
      icon: showing.placement == PlacementView.unknown
          ? AppIcons.warningAmber
          : AppIcons.info,
      tone: showing.placement == PlacementView.unknown
          ? context.status.warning.l800
          : context.text.l700,
    );
  }
}

/// The running World's control — the reference client's solid split button:
/// one bright green slab, the play mark and the state on its left, the
/// browser handoff on the right of a hairline. Both segments are the same
/// act — go to the running World — because the split is the reference
/// anatomy, not two behaviors; but each half answers the pointer alone,
/// brightening under it while its sibling rests.
///
/// The sheen is derived, not hand-tuned: [Lit] surfaces under one
/// top-mounted directional light compute the gradient the reference
/// client's button wears, and retune themselves if the fill ever changes.
class _RunningControl extends StatefulWidget {
  const _RunningControl({required this.onOpen, required this.tooltip});

  final VoidCallback? onOpen;
  final String tooltip;

  @override
  State<_RunningControl> createState() => _RunningControlState();
}

class _RunningControlState extends State<_RunningControl> {
  double _hoverState = 0;
  double _hoverHandoff = 0;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final fill = context.status.success.l800;
    // reason: the slab is a vivid status green in both themes, so its ink is
    // white in both — no theme rung answers "white regardless of theme".
    final ink = TokenEscape.rawColor(0xFFFFFFFF);
    // The hovered segment lifts toward its own ink; Lit hands back the
    // animated 0..1 and imposes no look of its own.
    Color raised(double hover) => Color.lerp(fill, ink, hover * 0.12)!;
    // The ambient scene first — what the lighting workbench edits is what
    // this control wears — and the canonical scene only where no window
    // mounted one (a bare test harness).
    final scene = LightTheme.maybeOf(context) ?? kAstrolabeScene;
    return Semantics(
      button: true,
      label: 'Go to running World',
      child: Tooltip(
        message: widget.tooltip,
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Lit(
              scene: scene,
              baseColor: raised(_hoverState),
              curvature: 0.12,
              elevation: 3,
              borderRadius: t.radius.corner(
                topLeft: Space.xxs,
                bottomLeft: Space.xxs,
              ),
              onTap: widget.onOpen,
              onHoverChange: (value) => setState(() => _hoverState = value),
              child: Container(
                height: 40,
                padding: t.padding.symmetric(h: Space.xl3),
                alignment: Alignment.center,
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
                      'RUNNING',
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
            Container(
              width: t.stroke.xxs,
              height: 40,
              // reason: the seam splitting the slab is a shade of its own
              // fill, not a theme rung — a dark line on green in any theme.
              color: TokenEscape.rawColor(0x40000000),
            ),
            Lit(
              scene: scene,
              baseColor: raised(_hoverHandoff),
              curvature: 0.12,
              elevation: 3,
              borderRadius: t.radius.corner(
                topRight: Space.xxs,
                bottomRight: Space.xxs,
              ),
              onTap: widget.onOpen,
              onHoverChange: (value) =>
                  setState(() => _hoverHandoff = value),
              child: Container(
                height: 40,
                padding: t.padding.symmetric(h: Space.md),
                alignment: Alignment.center,
                child: Icon(
                  AppIcons.openInNew,
                  size: _worldActionGlyphSize,
                  color: ink,
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _LifecycleState extends StatelessWidget {
  const _LifecycleState({
    required this.label,
    required this.tone,
    this.icon,
    this.loading = false,
    this.large = false,
  });

  final String label;
  final Color tone;
  final IconData? icon;
  final bool loading;
  final bool large;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Semantics(
      label: label,
      liveRegion: loading,
      child: Container(
        height: 40,
        constraints: const BoxConstraints(minWidth: 124),
        padding: t.padding.symmetric(h: Space.xl3),
        decoration: BoxDecoration(
          color: tone.withValues(alpha: 0.10),
          border: Border.all(color: tone.withValues(alpha: 0.45)),
          borderRadius: t.radius.all(large ? Space.xxs : Space.sm),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          mainAxisAlignment: MainAxisAlignment.center,
          children: [
            if (loading)
              Progress.spinner(
                size: large ? ProgressSize.lg : ProgressSize.xs,
                color: tone,
              )
            else if (icon != null)
              Icon(
                icon,
                size: large ? _worldActionGlyphSize : 16,
                color: tone,
              ),
            t.gap.x(Space.sm),
            Text(
              large ? label.toUpperCase() : label,
              style: context.bodyStyle.copyWith(
                color: tone,
                fontSize:
                    large ? _worldActionGlyphSize : context.bodyStyle.fontSize,
                fontWeight: large ? FontWeight.w400 : FontWeight.w700,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _StatusReadout extends StatelessWidget {
  const _StatusReadout({
    required this.icon,
    required this.label,
    required this.value,
    this.tone,
    this.tooltip,
  });

  final IconData icon;
  final String label;
  final String value;
  final Color? tone;
  final String? tooltip;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final color = tone ?? context.text.l700;
    final readout = Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(icon, size: 18, color: color),
        t.gap.x(Space.sm),
        Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(label, style: context.factLabelStyle),
            t.gap.y(Space.xxs),
            Text(
              value,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: context.labelStyle.copyWith(
                color: color,
                fontWeight: FontWeight.w600,
              ),
            ),
          ],
        ),
      ],
    );
    return tooltip == null
        ? readout
        : Tooltip(message: tooltip, child: readout);
  }
}

class _Details extends StatelessWidget {
  const _Details({required this.showing});

  final LibraryRow showing;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final heads = _matchingHeads(ClientScope.watch(context), showing);
    final people = showing.people;

    // No head of its own: the tiers inside already say who is in the World
    // and who merely holds it, and a title above them would count what the
    // two lines beneath it count better.
    final glance = _InfoPanel(child: _People(people: people));
    final details = _InfoPanel(
      title: 'WORLD DETAILS',
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _Fact(
            label: 'OPENS AT',
            value: showing.opensAt ?? 'No entry path declared',
            mono: true,
          ),
          t.gap.y(Space.xl3),
          _Fact(
            label: 'STORE',
            value: showing.store ?? 'Not reported',
            mono: true,
          ),
        ],
      ),
    );
    final serving = _InfoPanel(
      title: 'SERVING NOW',
      child: heads.isEmpty
          ? Text(
              'No matching head has reported an address.',
              style: context.proseStyle,
            )
          : Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                for (final head in heads) ...[
                  Row(
                    children: [
                      Expanded(
                        child: Text(
                          head.origin ?? '${head.kind} head',
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: context.monoStyle,
                        ),
                      ),
                      t.gap.x(Space.md),
                      Badge(
                        label: head.owned ? 'local' : 'external',
                        variant: head.owned
                            ? BadgeVariant.success
                            : BadgeVariant.outline,
                        radius: Space.xs,
                      ),
                    ],
                  ),
                  t.gap.y(Space.sm),
                ],
              ],
            ),
    );

    // The glance card is its own column on the RIGHT — the reference
    // client's sidebar, on the reference client's side — and the
    // operational panels flow in the space to its left. Narrow panes stack,
    // the operational panels first.
    return LayoutBuilder(
      builder: (context, constraints) {
        if (constraints.maxWidth < 620) {
          return Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              details,
              t.gap.y(Space.xl3),
              serving,
              t.gap.y(Space.xl3),
              glance,
            ],
          );
        }
        return Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  details,
                  t.gap.y(Space.xl3),
                  serving,
                ],
              ),
            ),
            t.gap.x(Space.xl3),
            t.box.width(TokenEscape.rawSize(kGlanceWidth), child: glance),
          ],
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
  const _InfoPanel({this.title, required this.child});

  /// The panel's head, or none — a panel whose content already announces
  /// itself carries no second name above it.
  final String? title;
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
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (title != null) ...[
            Text(title!, style: context.factLabelStyle),
            t.gap.y(Space.xl3),
          ],
          child,
        ],
      ),
    );
  }
}

class _Fact extends StatelessWidget {
  const _Fact({
    required this.label,
    required this.value,
    this.mono = false,
  });

  final String label;
  final String value;
  final bool mono;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(label, style: context.factLabelStyle),
        t.gap.y(Space.xxs),
        Text(
          value,
          maxLines: mono ? 2 : 1,
          overflow: TextOverflow.ellipsis,
          style: mono ? context.monoStyle : context.bodyStyle,
        ),
      ],
    );
  }
}

List<HeadRow> _matchingHeads(ClientView view, LibraryRow row) => view.heads
    .where((head) => head.orbit == null || head.orbit == row.orbit)
    .toList();

({String label, String detail, IconData icon, Color tone}) _syncCopy(
  BuildContext context,
  LibraryRow row,
) {
  final detail = row.syncDetail ??
      switch (row.placement) {
        PlacementView.placed => 'The running World did not report a sync gate.',
        PlacementView.vacant => 'Sync is checked after the World is launched.',
        PlacementView.unknown =>
          'The Orbit could not be asked for sync status.',
      };
  return switch (row.syncState) {
    'pass' => (
        label: 'Up to date',
        detail: detail,
        icon: AppIcons.cloudDownload,
        tone: context.status.success.l800,
      ),
    'wait' => (
        label: 'Syncing',
        detail: detail,
        icon: AppIcons.refresh,
        tone: context.text.l900,
      ),
    'fail' => (
        label: 'Needs attention',
        detail: detail,
        icon: AppIcons.error,
        tone: context.status.warning.l800,
      ),
    'warn' => (
        label: 'Check sync',
        detail: detail,
        icon: AppIcons.warningAmber,
        tone: context.status.warning.l800,
      ),
    'skip' => (
        label: 'Not applicable',
        detail: detail,
        icon: AppIcons.info,
        tone: context.text.l700,
      ),
    _ => (
        label:
            row.placement == PlacementView.vacant ? 'Offline' : 'Not reported',
        detail: detail,
        icon: row.placement == PlacementView.unknown
            ? AppIcons.warningAmber
            : AppIcons.cloudDownload,
        tone: row.placement == PlacementView.unknown
            ? context.status.warning.l800
            : context.text.l700,
      ),
  };
}

enum _Lifecycle { opening, running, ready, unreachable, unavailable }

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
        description: 'Placing this Orbit and preparing its World head.',
        variant: BadgeVariant.solid,
        dot: BadgeDotTone.brand,
      ),
    _Lifecycle.running => (
        label: 'Running',
        description: 'This World is already placed and ready to view.',
        variant: BadgeVariant.success,
        dot: BadgeDotTone.success,
      ),
    _Lifecycle.ready => (
        label: 'Ready',
        description: 'Launch starts the World and hands it to your browser.',
        variant: BadgeVariant.outline,
        dot: BadgeDotTone.neutral,
      ),
    _Lifecycle.unreachable => (
        label: 'Could not ask',
        description:
            'The last good Library record is shown while the Space is unreachable.',
        variant: BadgeVariant.warning,
        dot: BadgeDotTone.warning,
      ),
    _Lifecycle.unavailable => (
        label: 'Unavailable',
        description: switch (row.unopenable) {
          Unopenable.unhosted =>
            'This build does not host a head for this World.',
          Unopenable.undeclared => 'This World has not declared an entry path.',
          null => 'This World cannot be opened from this device.',
        },
        variant: BadgeVariant.muted,
        dot: BadgeDotTone.neutral,
      ),
  };
}

_Lifecycle _lifecycle(ClientView view, LibraryRow row) {
  // Once placed, a browser handoff never changes the World-level state. One
  // person can have several Worlds running, so there is no cancel/stop state
  // at this layer.
  if (row.placement == PlacementView.placed) return _Lifecycle.running;
  if (_opening(view, row)) return _Lifecycle.opening;
  if (row.unopenable != null || row.opensAt == null) {
    return _Lifecycle.unavailable;
  }
  return switch (row.placement) {
    PlacementView.placed => _Lifecycle.running,
    PlacementView.vacant => _Lifecycle.ready,
    PlacementView.unknown => _Lifecycle.unreachable,
  };
}

bool _opening(ClientView view, LibraryRow row) {
  final path = row.opensAt;
  return path != null &&
      view.inFlight.contains(ActionKeys.open(row.orbit, path));
}

String _openTooltip(LibraryRow row, {required bool running}) {
  return switch (row.unopenable) {
    Unopenable.unhosted =>
      'This build hosts no head for that World, so there is nothing to open.',
    Unopenable.undeclared => 'This World has not declared where to open it.',
    null => running
        ? 'Take me to the running World'
        : 'Start this World and hand it to my browser',
  };
}

Color _accent(BuildContext context, LibraryRow row) => row.accent == null
    ? context.surface.l500
    // reason: the World owns this seed. Snapping it to Astrolabe's brand ramp
    // would replace a declaration with the client's opinion.
    : TokenEscape.rawColor(0xFF000000 | row.accent!.toInt());

String _name(LibraryRow row) => row.displayName ?? 'Unnamed Space';

String _ago(BigInt? lastOpened) {
  if (lastOpened == null) return 'Never';
  final then = DateTime.fromMillisecondsSinceEpoch(
    lastOpened.toInt() * 1000,
    isUtc: true,
  );
  final elapsed = DateTime.now().toUtc().difference(then);
  if (elapsed.isNegative) return 'In the future';
  if (elapsed.inMinutes < 1) return 'Just now';
  if (elapsed.inHours < 1) return _plural(elapsed.inMinutes, 'minute');
  if (elapsed.inDays < 1) return _plural(elapsed.inHours, 'hour');
  return _plural(elapsed.inDays, 'day');
}

String _plural(int count, String unit) =>
    count == 1 ? '1 $unit ago' : '$count ${unit}s ago';
