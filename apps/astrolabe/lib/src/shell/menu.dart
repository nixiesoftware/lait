/// The application menu, where the operating system keeps one of its own.
///
/// macOS gives every application a menu bar at the top of the screen, outside
/// and above all of its windows. That is where this client's name, its
/// settings, and the standard window verbs belong — so on macOS the window
/// draws no wordmark and this declares the bar instead. Everywhere else the
/// wordmark in the caption is the only application menu there is, and this
/// widget is a pass-through.
///
/// ## Declaring the bar replaces it whole
///
/// Flutter's macOS embedder assigns `NSApp.mainMenu` from exactly what is
/// declared here — the nib's menus are not merged in, they are gone. So the
/// standard application menu is declared here too, as
/// [PlatformProvidedMenuItem]s, which are the system's own items rather than
/// look-alikes: About and Quit are AppKit's, not ours to imitate.
///
/// Text editing needs no Edit menu to work: Flutter's own
/// `DefaultTextEditingShortcuts` carries copy, cut, paste and select-all on
/// macOS, and an item here would only take those keystrokes away from it.
library;

import 'package:flutter/material.dart' show ThemeMode;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import 'host.dart';
import 'window.dart';

class AstrolabeMenuBar extends StatelessWidget {
  const AstrolabeMenuBar({
    super.key,
    required this.themeMode,
    required this.onToggleTheme,
    required this.child,
  });

  final ThemeMode themeMode;
  final VoidCallback onToggleTheme;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    if (!systemCarriesApplicationMenu) return child;

    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    // The same refusal the drawn menu applies: a re-read already in flight is
    // an item that cannot be chosen, not one that queues a second read.
    final rereading = view.inFlight.contains(ActionKeys.refresh);

    return PlatformMenuBar(
      menus: [
        PlatformMenu(
          label: 'Astrolabe',
          menus: [
            PlatformMenuItemGroup(
              members: [
                PlatformProvidedMenuItem(
                  type: PlatformProvidedMenuItemType.about,
                ),
              ],
            ),
            PlatformMenuItemGroup(
              members: [
                PlatformProvidedMenuItem(
                  type: PlatformProvidedMenuItemType.servicesSubmenu,
                ),
              ],
            ),
            PlatformMenuItemGroup(
              members: [
                PlatformProvidedMenuItem(
                  type: PlatformProvidedMenuItemType.hide,
                ),
                PlatformProvidedMenuItem(
                  type: PlatformProvidedMenuItemType.hideOtherApplications,
                ),
                PlatformProvidedMenuItem(
                  type: PlatformProvidedMenuItemType.showAllApplications,
                ),
              ],
            ),
            PlatformMenuItemGroup(
              members: [
                // Quitting is not closing: this is the item that stops the
                // client and everything it supervises, and the red traffic
                // light deliberately is not.
                PlatformProvidedMenuItem(
                  type: PlatformProvidedMenuItemType.quit,
                ),
              ],
            ),
          ],
        ),
        PlatformMenu(
          label: 'Client',
          menus: [
            PlatformMenuItem(
              label: 'Refresh local state',
              // `⌘R` is what a Mac reaches for; `F5` still works, because the
              // shell's own shortcut is unchanged and this adds to it.
              shortcut:
                  const SingleActivator(LogicalKeyboardKey.keyR, meta: true),
              onSelected: rereading
                  ? null
                  : () => client.dispatch(const ActionRequest.refresh()),
            ),
            PlatformMenuItem(
              label: themeMode == ThemeMode.dark
                  ? 'Use light theme'
                  : 'Use dark theme',
              onSelected: onToggleTheme,
            ),
          ],
        ),
        PlatformMenu(
          label: 'Window',
          menus: [
            PlatformMenuItemGroup(
              members: [
                PlatformMenuItem(
                  label: 'Displays',
                  shortcut: const SingleActivator(
                    LogicalKeyboardKey.keyD,
                    meta: true,
                    shift: true,
                  ),
                  onSelected: summonDisplays,
                ),
                PlatformMenuItem(
                  label: 'Address book',
                  shortcut: const SingleActivator(
                    LogicalKeyboardKey.keyB,
                    meta: true,
                    shift: true,
                  ),
                  onSelected: summonBook,
                ),
              ],
            ),
            PlatformMenuItemGroup(
              members: [
                // Minimise and zoom left the window when the traffic lights
                // took them back; this is where macOS keeps them for the
                // keyboard.
                PlatformProvidedMenuItem(
                  type: PlatformProvidedMenuItemType.minimizeWindow,
                ),
                PlatformProvidedMenuItem(
                  type: PlatformProvidedMenuItemType.zoomWindow,
                ),
                PlatformProvidedMenuItem(
                  type: PlatformProvidedMenuItemType.toggleFullScreen,
                ),
              ],
            ),
          ],
        ),
      ],
      child: child,
    );
  }
}
