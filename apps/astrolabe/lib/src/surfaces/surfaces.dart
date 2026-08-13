/// The surfaces a person can be looking at, and the page that draws one.
///
/// Ordered as they are drawn, and `library` is first because it is the front
/// page — a person with one identity and several Spaces must never open onto a
/// process inventory. That ordering is a product decision rather than an
/// alphabetical accident, and it is the order `Ctrl+1`–`Ctrl+7` follow.
library;

import 'package:flutter/widgets.dart';

import 'devices.dart';
import 'diagnostics.dart';
import 'heads.dart';
import 'library.dart';
import 'members.dart';
import 'spaces.dart';
import 'storage.dart';

enum Surface {
  library('Library'),
  spaces('Spaces'),
  members('Members'),
  devices('Devices'),
  heads('Heads'),
  storage('Storage'),
  diagnostics('Diagnostics');

  const Surface(this.title);

  final String title;
}

/// The page margin. A surface flush against the window edge reads as
/// unfinished whatever else is right about it.
const EdgeInsets kPageMargin = EdgeInsets.fromLTRB(16, 12, 16, 16);

class SurfacePage extends StatelessWidget {
  const SurfacePage({super.key, required this.surface});

  final Surface surface;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: kPageMargin,
      child: switch (surface) {
        Surface.library => const LibrarySurface(),
        Surface.spaces => const SpacesSurface(),
        Surface.members => const MembersSurface(),
        Surface.devices => const DevicesSurface(),
        Surface.heads => const HeadsSurface(),
        Surface.storage => const StorageSurface(),
        Surface.diagnostics => const DiagnosticsSurface(),
      },
    );
  }
}
