/// The window: a compact utility tier over the Library.
///
/// The utility tier is the draggable title bar: Astrolabe's identity block
/// opens the application menu, while address-book access and the window
/// controls stay flush with the opposite corner. There is no navigation
/// tier beneath it — the client IS the Library, the address book is its own
/// window, and each World carries its own settings. The client surfaces
/// lifecycle; everything else was a destination it had no business being.
library;

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/material.dart' show ThemeMode;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import '../surfaces/library.dart';
import 'host.dart';
import 'lighting.dart';
import 'menu.dart';
import 'record.dart';
import 'type.dart';
import 'window.dart';

/// The utility tier's height. Secondary windows retain the roomier single
/// 48-pixel caption supplied by [AstrolabeWindowFrame].
const double kUtilityBarHeight = 32;

class AstrolabeShell extends StatelessWidget {
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
  Widget build(BuildContext context) {
    // The lighting workbench wraps the whole window: its scene is the one
    // every lit surface reads, and in debug builds its panel floats over
    // the corner so the rules can be tuned against the real controls.
    //
    // The menu bar wraps that, because on macOS these settings are not in the
    // window at all — they are on the screen above it. Off macOS it draws
    // nothing and the wordmark below carries them.
    return AstrolabeMenuBar(
      themeMode: themeMode,
      onToggleTheme: onToggleTheme,
      child: LightingWorkbench(
        child: Shortcuts(
          shortcuts: const <ShortcutActivator, Intent>{
            SingleActivator(LogicalKeyboardKey.f5): _Reread(),
          },
          child: Actions(
            actions: <Type, Action<Intent>>{
              _Reread: CallbackAction<_Reread>(
                onInvoke: (_) => ClientScope.of(context)
                    .dispatch(const ActionRequest.refresh()),
              ),
            },
            child: Focus(
              autofocus: true,
              child: AstrolabeWindowFrame.primary(
                closePolicy: AstrolabeWindowClosePolicy.hide,
                chrome: chrome,
                captionHeight: kUtilityBarHeight,
                captionBottomBorder: false,
                // The same fact the HWND is configured with in `main`: no
                // maximise control, and a double-click on the caption that
                // does nothing rather than zooming what the button refuses.
                maximisable: kClientMaximisable,
                // Drawn only where the window carries the application menu;
                // on macOS the frame leaves this slot alone and the screen's
                // own bar holds what it opened.
                wordmark: _SettingsMenu(
                  themeMode: themeMode,
                  onToggleTheme: onToggleTheme,
                ),
                // The caption's middle carries nothing: the address book moved
                // to the operational bar, beside the other facts this device
                // holds about itself. Still a builder rather than a title —
                // a title would draw a rule and repeat the word the wordmark
                // to its left already says.
                captionBuilder: (context, constraints) =>
                    const SizedBox.shrink(),
                body: const Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    Expanded(child: LibrarySurface()),
                    // System truth stays visible. The bar carries the latest
                    // action or refusal without growing into a stack that
                    // steals height from the work above it.
                    OperationalBar(),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
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
              icon: AppIcons.cable,
              label: 'Displays',
              onTap: summonDisplays,
            ),
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
