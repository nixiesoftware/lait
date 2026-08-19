/// Window creation and chrome, behind one module.
///
/// Surfaces do not know which mechanism hosts them. Today that mechanism is
/// `desktop_multi_window` (engine per window). When Flutter's official
/// windowing API reaches stable, this file is the one that changes.
library;

import 'package:desktop_multi_window/desktop_multi_window.dart';
import 'package:flutter/services.dart';

/// The argument and stable host key that mark the address-book window.
///
/// One window, never one per summons: [summonBook] focuses an existing
/// engine that already carries this argument.
const bookWindowArgument = 'astrolabe.book';
const bookWindowKey = 'address-book';
const displaysWindowArgument = 'astrolabe.displays';
const displaysWindowKey = 'displays';
const correspondenceWindowArgument = 'astrolabe.correspondence';
const correspondenceWindowKey = 'correspondence';

/// One typed request for an owned top-level window.
///
/// All engine-per-window creation goes through this value and
/// [summonOwnedWindow]. Native ownership and non-client policy are installed by
/// the runner's global creation callback, independently of a surface's Dart
/// implementation.
class OwnedWindowRoute {
  const OwnedWindowRoute({
    required this.key,
    required this.arguments,
  });

  const OwnedWindowRoute.addressBook()
      : key = bookWindowKey,
        arguments = bookWindowArgument;

  const OwnedWindowRoute.displays()
      : key = displaysWindowKey,
        arguments = displaysWindowArgument;

  const OwnedWindowRoute.correspondence()
      : key = correspondenceWindowKey,
        arguments = correspondenceWindowArgument;

  final String key;
  final String arguments;

  bool matches(String candidate) => candidate == arguments;
}

bool isBookWindow(String arguments) =>
    arguments == bookWindowArgument ||
    arguments.split(RegExp(r'[\s,]')).contains(bookWindowArgument);

bool isDisplaysWindow(String arguments) =>
    arguments == displaysWindowArgument ||
    arguments.split(RegExp(r'[\s,]')).contains(displaysWindowArgument);

bool isCorrespondenceWindow(String arguments) =>
    arguments == correspondenceWindowArgument ||
    arguments.split(RegExp(r'[\s,]')).contains(correspondenceWindowArgument);

/// A sub-engine created by `desktop_multi_window` receives
/// `["multi_window", windowId, windowArgument]` on `main`, *before*
/// [WindowController.fromCurrentEngine] can answer. Routing must
/// read that list; asking the plugin first is how the book window
/// used to fall through into `window_manager` and paint nothing.
bool isBookEngine(List<String> argv) => argv.contains(bookWindowArgument);
bool isDisplaysEngine(List<String> argv) =>
    argv.contains(displaysWindowArgument);
bool isCorrespondenceEngine(List<String> argv) =>
    argv.contains(correspondenceWindowArgument);

bool isSubEngine(List<String> argv) =>
    argv.isNotEmpty && argv.first == 'multi_window';

/// The runner's summon channel. The plugin's `show()` is `SW_SHOW` alone —
/// it neither restores a minimised window nor raises an occluded one — so
/// the runner remembers the sub-window and restores + foregrounds it.
const MethodChannel _windowHost = MethodChannel('astrolabe/window_host');

/// A summons already being answered. The caption button is not an
/// [ActionRequest], so it has no in-flight key; two fast presses in the
/// async gap before the window registers would otherwise both `create()`.
final Set<String> _summoning = <String>{};

/// Open the book, or focus it if it is already open.
///
/// Closing the book closes a window, never a peer. A second summons does
/// not create a second engine.
Future<void> summonBook() =>
    summonOwnedWindow(const OwnedWindowRoute.addressBook());

/// Open display coordination, or focus it if it is already open.
Future<void> summonDisplays() =>
    summonOwnedWindow(const OwnedWindowRoute.displays());

/// Open the correspondence desk, or focus it if it is already open.
Future<void> summonCorrespondence() =>
    summonOwnedWindow(const OwnedWindowRoute.correspondence());

/// Open an owned window, or restore and focus the matching instance.
///
/// Windows remain hidden until their secondary frame supplies title, geometry,
/// theme and minimum-size policy to the native host. That prevents a flash of
/// the plugin's unconfigured system frame.
Future<void> summonOwnedWindow(OwnedWindowRoute route) async {
  if (!_summoning.add(route.key)) {
    return;
  }
  try {
    for (final controller in await WindowController.getAll()) {
      if (route.matches(controller.arguments)) {
        await _raiseWhenReady(route.key);
        return;
      }
    }
    await WindowController.create(
      WindowConfiguration(
        hiddenAtLaunch: true,
        arguments: route.arguments,
      ),
    );
    await _raiseWhenReady(route.key);
  } finally {
    _summoning.remove(route.key);
  }
}

Future<void> _raiseWhenReady(String key) async {
  // The child engine configures itself after its first frame. Keep duplicate
  // summons coalesced while that happens; configure_owned also reveals the
  // window, so timing out here never strands it hidden.
  for (var attempt = 0; attempt < 80; attempt += 1) {
    try {
      final raised = await _windowHost.invokeMethod<bool>('summon_owned', key);
      if (raised ?? false) return;
    } on PlatformException {
      return;
    } on MissingPluginException {
      return;
    }
    await Future<void>.delayed(const Duration(milliseconds: 25));
  }
}
