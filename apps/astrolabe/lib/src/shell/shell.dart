/// The window: one bar across the top, and a page under it.
///
/// The bar is the title bar. It carries primary navigation as pills on the
/// left, the one action that belongs on chrome rather than on a page in the
/// middle-right, and the window's own three controls flush with the corner.
/// Its contents are inset to the page margin so the first nav item lines up
/// with the first word of every surface below it; the bar itself is full bleed,
/// because a header inset from the window edge reads as a row of controls that
/// happen to be at the top rather than as chrome.
library;

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/material.dart' show ThemeMode;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import '../surfaces/surfaces.dart';
import 'record.dart';
import 'type.dart';
import 'window.dart';

/// The compact contextual band used by Operations.
const double kOperationsBarHeight = 34;

class AstrolabeShell extends StatefulWidget {
  const AstrolabeShell({
    super.key,
    required this.themeMode,
    required this.onToggleTheme,
  });

  final ThemeMode themeMode;
  final VoidCallback onToggleTheme;

  @override
  State<AstrolabeShell> createState() => _AstrolabeShellState();
}

class _AstrolabeShellState extends State<AstrolabeShell> {
  Surface _surface = Surface.library;

  @override
  Widget build(BuildContext context) {
    return Shortcuts(
      shortcuts: <ShortcutActivator, Intent>{
        for (final (index, surface) in Surface.values.indexed)
          SingleActivator(_digits[index], control: true): _ShowSurface(surface),
        const SingleActivator(LogicalKeyboardKey.f5): const _Reread(),
      },
      child: Actions(
        actions: <Type, Action<Intent>>{
          _ShowSurface: CallbackAction<_ShowSurface>(
            onInvoke: (intent) => setState(() => _surface = intent.surface),
          ),
          _Reread: CallbackAction<_Reread>(
            onInvoke: (_) =>
                ClientScope.of(context).dispatch(const ActionRequest.refresh()),
          ),
        },
        child: Focus(
          autofocus: true,
          child: AstrolabeWindowFrame(
            closePolicy: AstrolabeWindowClosePolicy.hide,
            wordmarkMinWidth: 820,
            captionBuilder: (context, constraints) => _PrimaryCaption(
              surface: _surface,
              showThemeLabel: constraints.maxWidth >= 760,
              themeMode: widget.themeMode,
              onToggleTheme: widget.onToggleTheme,
              onSurface: (surface) => setState(() => _surface = surface),
            ),
            body: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                if (_operationSurfaces.contains(_surface))
                  _OperationsBar(
                    surface: _surface,
                    onSurface: (surface) => setState(() => _surface = surface),
                  ),
                Expanded(child: SurfacePage(surface: _surface)),
                // System truth stays visible in every surface. The bar carries
                // the latest action or refusal without growing into a stack
                // that steals height from the work above it.
                const OperationalBar(),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

const List<LogicalKeyboardKey> _digits = [
  LogicalKeyboardKey.digit1,
  LogicalKeyboardKey.digit2,
  LogicalKeyboardKey.digit3,
  LogicalKeyboardKey.digit4,
  LogicalKeyboardKey.digit5,
  LogicalKeyboardKey.digit6,
  LogicalKeyboardKey.digit7,
];

class _ShowSurface extends Intent {
  const _ShowSurface(this.surface);
  final Surface surface;
}

class _Reread extends Intent {
  const _Reread();
}

class _PrimaryCaption extends StatelessWidget {
  const _PrimaryCaption({
    required this.surface,
    required this.showThemeLabel,
    required this.themeMode,
    required this.onToggleTheme,
    required this.onSurface,
  });

  final Surface surface;
  final bool showThemeLabel;
  final ThemeMode themeMode;
  final VoidCallback onToggleTheme;
  final ValueChanged<Surface> onSurface;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    final rereading = view.inFlight.contains(ActionKeys.refresh);

    return Row(
      children: [
        _PrimaryDestination(
          label: 'Library',
          selected: surface == Surface.library,
          onPressed: () => onSurface(Surface.library),
        ),
        _PrimaryDestination(
          label: 'Spaces',
          selected: surface == Surface.spaces,
          onPressed: () => onSurface(Surface.spaces),
        ),
        _PrimaryDestination(
          label: 'Members',
          selected: surface == Surface.members,
          onPressed: () => onSurface(Surface.members),
        ),
        _PrimaryDestination(
          label: 'Operations',
          selected: _operationSurfaces.contains(surface),
          onPressed: () => onSurface(Surface.devices),
        ),
        const Spacer(),
        Button(
          onPressed: rereading
              ? null
              : () => client.dispatch(const ActionRequest.refresh()),
          icon: AppIcons.refresh,
          semanticLabel: 'Refresh',
          isLoading: rereading,
          variant: ButtonVariant.ghost,
          size: ButtonSize.iconSm,
          tooltip: 'Read this machine again (F5)',
        ),
        t.gap.x(Space.xs),
        Button(
          onPressed: onToggleTheme,
          icon: themeMode == ThemeMode.dark
              ? AppIcons.toggleOn
              : AppIcons.toggleOff,
          label: showThemeLabel
              ? (themeMode == ThemeMode.dark ? 'Light' : 'Dark')
              : null,
          semanticLabel: themeMode == ThemeMode.dark
              ? 'Use light theme'
              : 'Use dark theme',
          variant: ButtonVariant.ghost,
          size: showThemeLabel ? ButtonSize.sm : ButtonSize.iconSm,
          tooltip: themeMode == ThemeMode.dark
              ? 'Use light theme'
              : 'Use dark theme',
        ),
        t.gap.x(Space.md),
      ],
    );
  }
}

const Set<Surface> _operationSurfaces = {
  Surface.devices,
  Surface.heads,
  Surface.storage,
  Surface.diagnostics,
};

class _PrimaryDestination extends StatelessWidget {
  const _PrimaryDestination({
    required this.label,
    required this.selected,
    required this.onPressed,
  });

  final String label;
  final bool selected;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    return Button(
      onPressed: onPressed,
      label: label,
      active: selected,
      variant: ButtonVariant.ghost,
      size: ButtonSize.sm,
    );
  }
}

class _OperationsBar extends StatelessWidget {
  const _OperationsBar({required this.surface, required this.onSurface});

  final Surface surface;
  final ValueChanged<Surface> onSurface;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return t.box.height(
      TokenEscape.rawSize(kOperationsBarHeight),
      child: Container(
        padding: t.padding.symmetric(h: Space.xl3),
        decoration: BoxDecoration(
          color: context.surface.l50,
          border: t.stroke.edge(bottom: context.border.l500),
        ),
        child: Row(
          children: [
            Text('OPERATIONS', style: context.factLabelStyle),
            t.gap.x(Space.xl3),
            for (final candidate in _operationSurfaces) ...[
              Button(
                onPressed: () => onSurface(candidate),
                label: candidate.title,
                active: candidate == surface,
                variant: ButtonVariant.ghost,
                size: ButtonSize.xs,
              ),
              t.gap.x(Space.xs),
            ],
          ],
        ),
      ),
    );
  }
}
