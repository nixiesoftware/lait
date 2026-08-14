/// The front page: a passive Library rail and one selected World.
///
/// Steam supplies the durable library spine; GOG supplies the selected-item
/// hierarchy. Astrolabe keeps both honest: selection only changes what is
/// drawn, while `Open` or a World-declared route is the act that can place an
/// Orbit and hand it to the browser.
library;

import 'package:covalence/covalence.dart' hide Surface;
import 'package:covalence/covalence.dart' as cv show Surface;
import 'package:covalence/listtile.dart';
import 'package:flutter/material.dart' show Theme;
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import '../settings/window.dart';
import '../shell/type.dart';

const double kRailWidth = 224;
const double kHeroHeight = 196;
const double _worldActionGlyphSize = 24;

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
            'Found a Space, or enter one from an invite, on the Spaces tab.',
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
              if (showing.routes.isNotEmpty) ...[
                _Routes(showing: showing),
                t.gap.y(Space.xl3),
              ],
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

class _Routes extends StatelessWidget {
  const _Routes({required this.showing});

  final LibraryRow showing;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);

    return Container(
      padding: t.padding.symmetric(h: Space.xl3, v: Space.sm),
      decoration: BoxDecoration(
        color: context.surface.l100,
        border: Border.all(color: context.border.l500, width: t.stroke.xxs),
        borderRadius: t.radius.all(Space.md),
      ),
      child: Row(
        children: [
          Text('WORLD', style: context.factLabelStyle),
          t.gap.x(Space.xl3),
          Expanded(
            child: Wrap(
              spacing: t.size.xs,
              runSpacing: t.size.xs,
              children: [
                for (final route in showing.routes)
                  Button(
                    onPressed: view.inFlight.contains(
                      ActionKeys.open(showing.orbit, route.path),
                    )
                        ? null
                        : () => client.dispatch(
                              ActionRequest.open(
                                orbit: showing.orbit,
                                entryPath: route.path,
                              ),
                            ),
                    label: route.label,
                    size: ButtonSize.xs,
                    variant: ButtonVariant.ghost,
                    tooltip: route.path,
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
      return Wrap(
        spacing: t.size.md,
        runSpacing: t.size.md,
        crossAxisAlignment: WrapCrossAlignment.center,
        children: [
          _LifecycleState(
            label: 'Running',
            icon: AppIcons.checkCircle,
            tone: context.status.success.l800,
            large: true,
          ),
          Button(
            onPressed: onOpen,
            semanticLabel: 'Go to running World',
            icon: AppIcons.openInNew,
            variant: ButtonVariant.ghost,
            size: ButtonSize.icon,
            tooltip: _openTooltip(showing, running: true),
          ),
        ],
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
              'Launch',
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
          borderRadius: t.radius.all(Space.sm),
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
              label,
              style: context.bodyStyle.copyWith(
                color: tone,
                fontSize:
                    large ? _worldActionGlyphSize : context.bodyStyle.fontSize,
                fontWeight: FontWeight.w700,
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

    return LayoutBuilder(
      builder: (context, constraints) {
        final split = constraints.maxWidth >= 620;
        final width = split
            ? (constraints.maxWidth - t.size.xl3) / 2
            : constraints.maxWidth;
        return Wrap(
          spacing: t.size.xl3,
          runSpacing: t.size.xl3,
          children: [
            SizedBox(
              width: width,
              child: _InfoPanel(
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
              ),
            ),
            SizedBox(
              width: width,
              child: _InfoPanel(
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
              ),
            ),
          ],
        );
      },
    );
  }
}

class _InfoPanel extends StatelessWidget {
  const _InfoPanel({required this.title, required this.child});

  final String title;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Container(
      padding: t.padding.all(Space.xl3),
      decoration: BoxDecoration(
        color: context.surface.l50,
        border: Border.all(color: context.border.l500, width: t.stroke.xxs),
        borderRadius: t.radius.all(Space.md),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(title, style: context.factLabelStyle),
          t.gap.y(Space.xl3),
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
