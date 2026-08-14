/// Window creation and chrome, behind one module.
///
/// Surfaces do not know which mechanism hosts them. Today that mechanism is
/// `desktop_multi_window` (engine per window). When Flutter's official
/// windowing API reaches stable, this file is the one that changes.
library;

import 'package:desktop_multi_window/desktop_multi_window.dart';
import 'package:flutter/services.dart';

/// The argument that marks the address-book window.
///
/// One window, never one per summons: [summonBook] focuses an existing
/// engine that already carries this argument.
const bookWindowArgument = 'astrolabe.book';

bool isBookWindow(String arguments) =>
    arguments == bookWindowArgument ||
    arguments.split(RegExp(r'[\s,]')).contains(bookWindowArgument);

/// A sub-engine created by `desktop_multi_window` receives
/// `["multi_window", windowId, windowArgument]` on `main`, *before*
/// [WindowController.fromCurrentEngine] can answer. Routing must
/// read that list; asking the plugin first is how the book window
/// used to fall through into `window_manager` and paint nothing.
bool isBookEngine(List<String> argv) => argv.contains(bookWindowArgument);

bool isSubEngine(List<String> argv) =>
    argv.isNotEmpty && argv.first == 'multi_window';

/// The runner's summon channel. The plugin's `show()` is `SW_SHOW` alone —
/// it neither restores a minimised window nor raises an occluded one — so
/// the runner remembers the sub-window and restores + foregrounds it.
const MethodChannel _summon = MethodChannel('astrolabe/window_summon');

/// A summons already being answered. The caption button is not an
/// [ActionRequest], so it has no in-flight key; two fast presses in the
/// async gap before the window registers would otherwise both `create()`.
bool _summoning = false;

/// Open the book, or focus it if it is already open.
///
/// Closing the book closes a window, never a peer. A second summons does
/// not create a second engine.
Future<void> summonBook() async {
  if (_summoning) {
    return;
  }
  _summoning = true;
  try {
    for (final controller in await WindowController.getAll()) {
      if (isBookWindow(controller.arguments)) {
        await controller.show();
        await _raiseBook();
        return;
      }
    }
    final created = await WindowController.create(
      WindowConfiguration(
        hiddenAtLaunch: true,
        arguments: bookWindowArgument,
      ),
    );
    await created.show();
    await _raiseBook();
  } finally {
    _summoning = false;
  }
}

Future<void> _raiseBook() async {
  try {
    await _summon.invokeMethod<bool>('summon_book');
  } on PlatformException {
    // The window vanished between the scan and the raise; shown is enough.
  } on MissingPluginException {
    // Tests and non-Windows shells have no runner channel.
  }
}
