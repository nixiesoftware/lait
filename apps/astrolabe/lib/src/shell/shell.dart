/// The window: a compact utility tier, primary navigation, and a page.
///
/// The upper tier is the draggable title bar: Astrolabe's identity block opens
/// the application menu, while address-book access and the window controls stay
/// flush with the opposite corner. The lower tier carries primary navigation as
/// a menu bar of buttons — the current destination is a held-down fill, not an
/// underline. This is deliberately the same hierarchy as a desktop client such
/// as Steam rather than a web page toolbar.
library;

import 'package:covalence/covalence.dart' hide Surface;
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
const double kPrimaryBarHeight = 40;

class AstrolabeShell extends StatefulWidget {
  const AstrolabeShell({
    super.key,
    required this.themeMode,
    required this.onToggleTheme,
    this.chrome = const ManagerWindowControlHost(),
  });

  final ThemeMode themeMode;
  final VoidCallback onToggleTheme;
  final WindowControlHost chrome;

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
          child: AstrolabeWindowFrame.primary(
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
      width: const PortalWidth.fixed(336),
      triggerBuilder: (context, _, onTap) => Button(
        onPressed: onTap,
        semanticLabel: 'Astrolabe settings',
        variant: ButtonVariant.ghost,
        size: ButtonSize.sm,
        minTapTarget: kUtilityBarHeight,
        style: const Style([$Pad.symmetric(h: Space.zero)]),
        child: Text(
          'ASTROLABE',
          style: context.bodyStyle.copyWith(
            color: context.brand.l800,
            fontWeight: FontWeight.w700,
            letterSpacing: 0.35,
          ),
        ),
      ),
      itemsBuilder: (_) => [
        MenuLabel(
          child: _SettingsIdentityHeader(
            version: view.host?.version,
          ),
        ),
        const MenuDivider(),
        MenuSection(
          label: 'CLIENT SETTINGS',
          children: [
            MenuItem(
              icon: AppIcons.refresh,
              label: 'Refresh local state',
              shortcut: 'F5',
              enabled: !rereading,
              onTap: rereading
                  ? null
                  : () => client.dispatch(const ActionRequest.refresh()),
            ),
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
        ),
      ],
    );
  }
}

class _SettingsIdentityHeader extends StatelessWidget {
  const _SettingsIdentityHeader({
    required this.version,
  });

  final String? version;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: context.tokens.padding.all(Space.md),
      child: Row(
        children: [
          Expanded(
            child: Text(
              'ASTROLABE',
              style: context.headingStyle.copyWith(
                color: context.brand.l800,
              ),
            ),
          ),
          if (version != null)
            Text(
              'v$version',
              style: context.monoStyle.copyWith(color: context.text.l700),
            ),
        ],
      ),
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

/// Locates the primary navigation tier from tests without exporting its type.
const Key kPrimaryNavigationKey = ValueKey<String>('primary-navigation');

class _PrimaryNavigation extends StatelessWidget {
  const _PrimaryNavigation({required this.surface, required this.onSurface});

  final Surface surface;
  final ValueChanged<Surface> onSurface;

  /// The four primary destinations. Operations fronts a family, so its button
  /// stays held down while any of that family's surfaces is current.
  static const List<(Surface, String)> _destinations = [
    (Surface.library, 'Library'),
    (Surface.spaces, 'Spaces'),
    (Surface.members, 'Members'),
    (Surface.devices, 'Operations'),
  ];

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final current =
        _operationSurfaces.contains(surface) ? Surface.devices : surface;
    return SizedBox(
      height: kPrimaryBarHeight,
      child: Container(
        key: kPrimaryNavigationKey,
        // A button carries its own horizontal padding (Space.xl at sm), so the
        // bar contributes the remainder and the first label lands on the same
        // 16px gutter as the wordmark above and OPERATIONS below.
        padding: t.padding.symmetric(h: Space.xs),
        decoration: BoxDecoration(
          color: context.layer.bg,
          border: t.stroke.edge(bottom: context.layer.border),
        ),
        child: Row(
          children: [
            for (final (candidate, label) in _destinations) ...[
              _MenuBarButton(
                label: label,
                current: candidate == current,
                size: ButtonSize.sm,
                onPressed: () => onSurface(candidate),
              ),
              t.gap.x(Space.xs),
            ],
          ],
        ),
      ),
    );
  }
}

/// One destination in a menu bar: a ghost button whose *current* state is a
/// held-down fill. Not `Button.active` — this app opts out of focus rings
/// (`FocusRing.none`), which leaves that flag drawing nothing at all.
class _MenuBarButton extends StatelessWidget {
  const _MenuBarButton({
    required this.label,
    required this.current,
    required this.onPressed,
    this.size = ButtonSize.xs,
  });

  final String label;
  final bool current;
  final VoidCallback onPressed;
  final ButtonSize size;

  @override
  Widget build(BuildContext context) {
    return Button(
      onPressed: onPressed,
      label: label,
      variant: ButtonVariant.ghost,
      size: size,
      backgroundColor: current ? context.layer.bgActive : null,
      style: current ? const Style([$Typo(weight: FontWeight.w600)]) : null,
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
              _MenuBarButton(
                label: candidate.title,
                current: candidate == surface,
                onPressed: () => onSurface(candidate),
              ),
              t.gap.x(Space.xs),
            ],
          ],
        ),
      ),
    );
  }
}
