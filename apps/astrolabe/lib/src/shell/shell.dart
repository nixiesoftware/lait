/// The window: a compact utility tier, primary navigation, and a page.
///
/// The upper tier is the draggable title bar: Astrolabe's wordmark opens the
/// small application menu, while identity access and the window controls stay
/// flush with the opposite corner. The lower tier carries primary navigation
/// with a persistent active underline. This is deliberately the same hierarchy
/// as a desktop client such as Steam rather than a web page toolbar.
library;

import 'package:covalence/covalence.dart' hide Surface, WindowChrome;
import 'package:flutter/material.dart' show ThemeMode;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import '../surfaces/surfaces.dart';
import 'host.dart';
import 'record.dart';
import 'type.dart';
import 'window.dart';

/// The compact contextual band used by Operations.
const double kOperationsBarHeight = 34;

/// The two tiers of the primary client header. Secondary windows retain the
/// roomier single 48-pixel caption supplied by [AstrolabeWindowFrame].
const double kUtilityBarHeight = 32;
const double kPrimaryBarHeight = 44;

class AstrolabeShell extends StatefulWidget {
  const AstrolabeShell({
    super.key,
    required this.themeMode,
    required this.onToggleTheme,
    this.chrome = const ManagerWindowChrome(),
  });

  final ThemeMode themeMode;
  final VoidCallback onToggleTheme;
  final WindowChrome chrome;

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
            chrome: widget.chrome,
            captionHeight: kUtilityBarHeight,
            captionBottomBorder: false,
            wordmark: _SettingsMenu(
              themeMode: widget.themeMode,
              onToggleTheme: widget.onToggleTheme,
            ),
            captionBuilder: (context, constraints) => const _UtilityCaption(),
            body: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                _PrimaryNavigation(
                  surface: _surface,
                  onSurface: (surface) => setState(() => _surface = surface),
                ),
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

class _SettingsMenu extends StatelessWidget {
  const _SettingsMenu({
    required this.themeMode,
    required this.onToggleTheme,
  });

  final ThemeMode themeMode;
  final VoidCallback onToggleTheme;

  @override
  Widget build(BuildContext context) {
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    final rereading = view.inFlight.contains(ActionKeys.refresh);

    return DropdownMenu(
      side: PopoverSide.bottom,
      align: PopoverAlign.start,
      sideOffset: 4,
      width: const PortalWidth.fixed(220),
      triggerBuilder: (context, isOpen, onTap) => Button(
        onPressed: onTap,
        active: isOpen,
        semanticLabel: 'Astrolabe settings',
        tooltip: 'Astrolabe settings',
        variant: ButtonVariant.ghost,
        size: ButtonSize.sm,
        minTapTarget: kUtilityBarHeight,
        style: const Style([$Pad.symmetric(h: Space.zero)]),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(
              'ASTROLABE',
              style: context.factLabelStyle.copyWith(
                color: context.text.l950,
                fontWeight: FontWeight.w700,
                letterSpacing: 1.4,
              ),
            ),
            context.tokens.gap.x(Space.xs),
            Icon(
              AppIcons.arrowDropDown,
              size: 14,
              color: context.text.l900,
            ),
          ],
        ),
      ),
      itemsBuilder: (_) => [
        MenuItem(
          icon: AppIcons.refresh,
          label: 'Refresh',
          shortcut: 'F5',
          enabled: !rereading,
          onTap: rereading
              ? null
              : () => client.dispatch(const ActionRequest.refresh()),
        ),
        const MenuDivider(),
        MenuItem(
          icon: themeMode == ThemeMode.dark
              ? AppIcons.toggleOff
              : AppIcons.toggleOn,
          label: themeMode == ThemeMode.dark
              ? 'Use light theme'
              : 'Use dark theme',
          onTap: onToggleTheme,
        ),
      ],
    );
  }
}

class _UtilityCaption extends StatelessWidget {
  const _UtilityCaption();

  @override
  Widget build(BuildContext context) {
    return Row(
      mainAxisAlignment: MainAxisAlignment.end,
      children: [
        Button(
          onPressed: summonBook,
          icon: AppIcons.person,
          semanticLabel: 'Address book',
          variant: ButtonVariant.ghost,
          size: ButtonSize.iconSm,
          tooltip: 'Open the address book',
        ),
        context.tokens.gap.x(Space.sm),
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

class _PrimaryNavigation extends StatelessWidget {
  const _PrimaryNavigation({required this.surface, required this.onSurface});

  final Surface surface;
  final ValueChanged<Surface> onSurface;

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: kPrimaryBarHeight,
      child: Tabs<Surface>(
        value: _operationSurfaces.contains(surface) ? Surface.devices : surface,
        options: const [
          TabsOption(value: Surface.library, label: 'Library'),
          TabsOption(value: Surface.spaces, label: 'Spaces'),
          TabsOption(value: Surface.members, label: 'Members'),
          TabsOption(value: Surface.devices, label: 'Operations'),
        ],
        onChanged: onSurface,
      ),
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
