/// `astrolabe.exe` — the local client through which a person reaches the
/// Worlds their device serves.
///
/// A Dart interface over a Rust core. Everything below this file's import of
/// `src/core/client.dart` is Rust: process supervision, control-protocol
/// traffic, observation, and the single model of client state. This half draws
/// it and holds nothing but drafts.
library;

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/material.dart' show MaterialApp, Scaffold, ThemeMode;
import 'package:flutter/widgets.dart';
import 'package:lit_ui/lit_ui.dart' show LitShader;
import 'package:window_manager/window_manager.dart';

import 'src/core/client.dart';
import 'src/settings/window.dart';
import 'src/shell/book.dart';
import 'src/shell/displays.dart';
import 'src/shell/host.dart';
import 'src/shell/shell.dart';
import 'src/shell/theme.dart';
import 'src/shell/window.dart';

/// The client's whole range of shapes: it opens at its ceiling and the only
/// direction it moves is shorter.
///
/// Neither bound is decoration. A layout that only works at the size the window
/// happens to open at breaks the first time somebody drags a corner — and a
/// launcher that can be dragged across a 4K display is a page of emptiness
/// around one card, which is the same defect from the other end.
///
/// Width is *fixed*: floor and ceiling are both 640, because the client is a
/// rail and one detail column and neither has anything to do with more. Height
/// is the only axis with play, and only 40 of it — the client stacks a hero, an
/// action band and a card down one column, so vertical is what runs out first.
///
/// The ceiling is also the opening size on purpose. `waitUntilReadyToShow`
/// applies `size` before `maximumSize`, so a window asked to open wider than
/// its ceiling opens wide anyway and is only clamped at the next drag.
const Size _widest = Size(640, 720);
const Size _narrowest = Size(640, 680);
Future<void> main(List<String> arguments) async {
  WidgetsFlutterBinding.ensureInitialized();

  // The lighting shader, once, before anything lit is drawn: with it loaded
  // a Lit surface with a material renders per-pixel PBR; without it every
  // window falls back to the canvas approximation for its whole life.
  await LitShader.load();

  // Sub-engines get argv from the plugin (`multi_window`, id, argument).
  // `fromCurrentEngine` is not ready yet on that isolate, and
  // `window_manager` is not registered there — asking either is how
  // this window used to stay white.
  if (isSubEngine(arguments)) {
    final settings = WorldSettingsSnapshot.fromArguments(arguments);
    if (settings != null) {
      runApp(WorldSettingsApp(snapshot: settings));
      return;
    }
    if (isDisplaysEngine(arguments)) {
      final client = await Client.start();
      runApp(DisplaysApp(client: client));
      return;
    }
    if (!isBookEngine(arguments)) {
      debugPrint('unknown sub-window arguments: $arguments');
      return;
    }
    // The OS window text is set natively at window creation (see
    // RegisterOwnedWindowChrome). The visible secondary frame configures the
    // contextual title and reveals the already-owned native window.
    final client = await Client.start();
    runApp(BookApp(client: client));
    return;
  }

  await windowManager.ensureInitialized();
  await showAstrolabeWindow(
    astrolabeWindowOptions(
      size: _widest,
      minimumSize: _narrowest,
      maximumSize: _widest,
      title: 'Astrolabe',
    ),
    maximisable: kClientMaximisable,
  );

  // The core comes up before the first frame, so nothing is ever drawn against
  // a client that does not exist yet.
  final client = await Client.start();
  runApp(Astrolabe(client: client));
}

class Astrolabe extends StatefulWidget {
  const Astrolabe({super.key, required this.client});

  final Client client;

  @override
  State<Astrolabe> createState() => _AstrolabeState();
}

class _AstrolabeState extends State<Astrolabe> {
  // Astrolabe is a desktop control room. It opens in the quieter dark register
  // even when Windows is light; the explicit switch in the caption is the
  // user's way back to the separately tuned light theme.
  ThemeMode _themeMode = ThemeMode.dark;

  void _toggleTheme() {
    setState(() {
      _themeMode =
          _themeMode == ThemeMode.dark ? ThemeMode.light : ThemeMode.dark;
    });
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Astrolabe',
      debugShowCheckedModeBanner: false,
      // covalence is Material-free; `MaterialApp` is here for the navigator and
      // the theme-extension plumbing its tokens ride on, and for nothing that
      // draws.
      theme: astrolabeTheme(Brightness.light),
      darkTheme: astrolabeTheme(Brightness.dark),
      themeMode: _themeMode,
      home: WorldSettingsScope(
        onOpen: launchWorldSettings,
        child: ClientScope(
          client: widget.client,
          child: ToastRegion(
            child: Scaffold(
              body: AstrolabeShell(
                themeMode: _themeMode,
                onToggleTheme: _toggleTheme,
              ),
            ),
          ),
        ),
      ),
    );
  }
}
