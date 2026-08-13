/// The type roles this client draws in.
///
/// covalence supplies the *scale* (`context.font.lg`) and the *faces*
/// (`context.appFonts.sans`) but not the roles that compose them, so this is
/// the one place they are composed. Everything else asks for a role by name.
///
/// A role rather than a size at the call site, for the same reason the design
/// system gives a colour a role: `context.font.lg` at a call site is a decision
/// nobody can find again, and forty of them are a type scale nobody can retune.
///
/// **This is a gap in covalence, not a design decision of this app.** A
/// typography role set belongs in the design system beside the buttons and the
/// cards, and it should move there — this file exists so the port did not stall
/// on it, and it is small on purpose so that moving it is a delete.
library;

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/widgets.dart';

extension TypeRoles on BuildContext {
  /// A page's name. One per surface, and never twice on a page.
  TextStyle get titleStyle => appFonts.sans.copyWith(
        fontSize: font.lg,
        fontWeight: FontWeight.w600,
        color: text.l950,
      );

  /// The name of a thing inside a page — a row, a card, a section.
  TextStyle get headingStyle => appFonts.sans.copyWith(
        fontSize: font.md,
        fontWeight: FontWeight.w600,
        color: text.l950,
      );

  /// What a person reads rather than glances at.
  TextStyle get bodyStyle =>
      appFonts.sans.copyWith(fontSize: font.sm, color: text.l950);

  /// A sentence explaining what a surface does or why something is empty.
  ///
  /// Quieter than body, and held one step higher than a label: prose is read in
  /// full sentences, and the floor a short word can live at is not the floor a
  /// paragraph can.
  TextStyle get proseStyle =>
      appFonts.sans.copyWith(fontSize: font.sm, color: text.l800);

  /// A short supporting word beside something — a state, a count, a mount.
  TextStyle get labelStyle =>
      appFonts.sans.copyWith(fontSize: font.xs, color: text.l800);

  /// The caption over a fact. Small, spaced, and quiet, so the fact under it is
  /// the thing being read.
  TextStyle get factLabelStyle => appFonts.sans.copyWith(
        fontSize: font.xxs,
        fontWeight: FontWeight.w500,
        letterSpacing: 0.6,
        color: text.l700,
      );

  /// A path, an address, an id — anything a person copies rather than reads.
  TextStyle get monoStyle =>
      appFonts.mono.copyWith(fontSize: font.xs, color: text.l900);
}
