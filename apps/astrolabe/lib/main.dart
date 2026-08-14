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
import 'package:window_manager/window_manager.dart';

import 'src/core/client.dart';
import 'src/settings/window.dart';
import 'src/shell/book.dart';
import 'src/shell/host.dart';
import 'src/shell/shell.dart';
import 'src/shell/theme.dart';
import 'src/shell/window.dart';

/// The window's own opening size, and the floor every layout has to survive.
///
/// The minimum is not decoration: a measurement that only works at the size the
/// window happens to open at is one that breaks the first time somebody drags a
/// corner.
const Size _opening = Size(1040, 720);
const Size _narrowest = Size(640, 480);
const Size _settingsOpening = Size(560, 680);
const Size _settingsNarrowest = Size(440, 520);

Future<void> main(List<String> arguments) async {
  WidgetsFlutterBinding.ensureInitialized();

  // World settings is still a separate process — not this windowing
  // mechanism. It never starts a second core.
  final settings = WorldSettingsSnapshot.fromArguments(arguments);
  if (settings != null) {
    await windowManager.ensureInitialized();
    await showAstrolabeWindow(
      astrolabeWindowOptions(
        size: _settingsOpening,
        minimumSize: _settingsNarrowest,
        title: '${settings.name} settings',
      ),
    );
    runApp(WorldSettingsApp(snapshot: settings));
    return;
  }

  // Sub-engines get argv from the plugin (`multi_window`, id, argument).
  // `fromCurrentEngine` is not ready yet on that isolate, and
  // `window_manager` is not registered there — asking either is how
  // this window used to stay white.
  if (isBookEngine(arguments)) {
    // The OS window text is set natively at window creation (see
    // RegisterWindowChrome): asking the chrome channel from here races the
    // handler's registration, and an await that throws before runApp is a
    // window that stays white.
    final client = await Client.start();
    runApp(BookApp(client: client));
    return;
  }

  if (isSubEngine(arguments)) {
    debugPrint('unknown sub-window arguments: $arguments');
    return;
  }

  await windowManager.ensureInitialized();
  await showAstrolabeWindow(
    astrolabeWindowOptions(
      size: _opening,
      minimumSize: _narrowest,
      title: 'Astrolabe',
    ),
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
