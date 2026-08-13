/// `astrolabe.exe` — the local client through which a person reaches the
/// Worlds their device serves.
///
/// A Dart interface over a Rust core. Everything below this file's import of
/// `src/core/client.dart` is Rust: process supervision, control-protocol
/// traffic, observation, and the single model of client state. This half draws
/// it and holds nothing but drafts.
library;

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/material.dart'
    show Brightness, MaterialApp, Scaffold, ThemeData, ThemeMode;
import 'package:flutter/widgets.dart';
import 'package:window_manager/window_manager.dart';

import 'src/core/client.dart';
import 'src/shell/shell.dart';

/// The window's own opening size, and the floor every layout has to survive.
///
/// The minimum is not decoration: a measurement that only works at the size the
/// window happens to open at is one that breaks the first time somebody drags a
/// corner.
const Size _opening = Size(1040, 720);
const Size _narrowest = Size(640, 480);

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await windowManager.ensureInitialized();

  await windowManager.waitUntilReadyToShow(
    const WindowOptions(
      size: _opening,
      minimumSize: _narrowest,
      center: true,
      title: 'Astrolabe',
      // No system title bar: this client draws its own caption. What that buys
      // is one surface at the top of the window instead of two in colours the
      // theme never agreed on.
      titleBarStyle: TitleBarStyle.hidden,
    ),
    () async {
      await windowManager.show();
      await windowManager.focus();
    },
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
      theme: _astrolabeTheme(Brightness.light),
      darkTheme: _astrolabeTheme(Brightness.dark),
      themeMode: _themeMode,
      home: ClientScope(
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
    );
  }
}

ThemeData _astrolabeTheme(Brightness brightness) => covalenceTheme(
      ThemeConfig(
        brightness: brightness,
        // reason: these are Astrolabe's two application-level seeds. Covalence
        // derives every surface, text, border, focus and brand rung from them.
        // The cool neutral keeps the dark shell blue-charcoal without pinning
        // a raw colour at any component call site.
        brandSeed: TokenEscape.rawColor(0xFF5B8DEF),
        neutralSeed: TokenEscape.rawColor(0xFF53667D),
      ),
    );
