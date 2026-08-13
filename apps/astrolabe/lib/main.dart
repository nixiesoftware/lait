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
    show MaterialApp, Scaffold, ThemeMode;
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

class Astrolabe extends StatelessWidget {
  const Astrolabe({super.key, required this.client});

  final Client client;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Astrolabe',
      debugShowCheckedModeBanner: false,
      // covalence is Material-free; `MaterialApp` is here for the navigator and
      // the theme-extension plumbing its tokens ride on, and for nothing that
      // draws.
      theme: covalenceTheme(const ThemeConfig()),
      themeMode: ThemeMode.system,
      home: ClientScope(
        client: client,
        child: const ToastRegion(child: Scaffold(body: AstrolabeShell())),
      ),
    );
  }
}
