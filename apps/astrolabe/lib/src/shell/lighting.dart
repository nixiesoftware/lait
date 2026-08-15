/// The lighting workbench — lit_ui's own runtime configuration utility,
/// mounted over the main window in debug builds.
///
/// The canonical scene below is the one fact; every lit surface resolves
/// through the ambient [LightTheme] first, so what the workbench edits is
/// what the real controls wear — a playground that tuned a copy would be a
/// second model of the lighting, agreeing with the client except when it
/// mattered. Release builds mount the same scene with no panel.
library;

import 'package:flutter/foundation.dart' show kDebugMode;
import 'package:flutter/widgets.dart';
import 'package:lit_ui/lit_ui.dart';

/// One light from straight above — the top-bright sheen the reference
/// client's slabs wear. The default every lit surface falls back to when no
/// [LightTheme] is in scope (a bare test harness, mostly).
const LightScene kAstrolabeScene = LightScene(
  lights: [DirectionalLight(angle: 0, intensity: 0.85)],
);

/// Wraps a window's body. In debug builds this is lit_ui's interactive
/// panel — the collapsed button in the bottom-right corner expands into
/// controls for adding, editing, and removing lights, materials, and
/// profiles at runtime. In release it collapses to the canonical scene,
/// panel-less, and costs nothing.
class LightingWorkbench extends StatelessWidget {
  const LightingWorkbench({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) => LightDebugOverlay(
        enabled: kDebugMode,
        defaultScene: kAstrolabeScene,
        child: child,
      );
}
