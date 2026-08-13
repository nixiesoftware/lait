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
  await windowManager.ensureInitialized();

  final settings = WorldSettingsSnapshot.fromArguments(arguments);
  final options = settings == null
      ? astrolabeWindowOptions(
          size: _opening,
          minimumSize: _narrowest,
          title: 'Astrolabe',
        )
      : astrolabeWindowOptions(
          size: _settingsOpening,
          minimumSize: _settingsNarrowest,
          title: '${settings.name} settings',
        );

  await showAstrolabeWindow(options);

  if (settings != null) {
    runApp(WorldSettingsApp(snapshot: settings));
    return;
  }

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
