import 'package:covalence/covalence.dart';
import 'package:flutter/material.dart' show Brightness, ThemeData;

ThemeData astrolabeTheme(Brightness brightness) => covalenceTheme(
      ThemeConfig(
        brightness: brightness,
        // reason: these are Astrolabe's two application-level seeds. Covalence
        // derives every surface, text, border, focus and brand rung from them.
        // The cool neutral keeps the dark shell blue-charcoal without pinning
        // a raw colour at any component call site.
        brandSeed: TokenEscape.rawColor(0xFF5B8DEF),
        neutralSeed: TokenEscape.rawColor(0xFF53667D),
        // Astrolabe draws no focus ring, by decision. `FocusRing.none` lands
        // after the seeded ring in the extension list, so it replaces it for
        // every control in every window.
        extraExtensions: const [FocusRing.none],
      ),
    );
