/// Window creation and chrome, behind one module.
///
/// Surfaces do not know which mechanism hosts them. Today that mechanism is
/// `desktop_multi_window` (engine per window). When Flutter's official
/// windowing API reaches stable, this file is the one that changes.
library;

import 'package:desktop_multi_window/desktop_multi_window.dart';

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

/// Open the book, or focus it if it is already open.
///
/// Closing the book closes a window, never a peer. A second summons does
/// not create a second engine.
Future<void> summonBook() async {
  for (final controller in await WindowController.getAll()) {
    if (isBookWindow(controller.arguments)) {
      await controller.show();
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
}
