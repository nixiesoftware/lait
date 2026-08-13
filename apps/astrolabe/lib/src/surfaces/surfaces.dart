/// The surfaces a person can be looking at, and the page that draws one.
///
/// Ordered as they are drawn, and `library` is first because it is the front
/// page — a person with one identity and several Spaces must never open onto a
/// process inventory. That ordering is a product decision rather than an
/// alphabetical accident, and it is the order `Ctrl+1`–`Ctrl+7` follow.
library;

import 'package:flutter/widgets.dart';

import '../shell/type.dart';
import 'library.dart';

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
        _ => _NotYetPorted(surface: surface),
      },
    );
  }
}

/// A surface whose Dart port has not landed.
///
/// Says which one and says why, rather than drawing an empty page: a blank
/// surface during a migration is indistinguishable from a surface that read
/// successfully and found nothing, and those are the two states this whole
/// interface is written to keep apart.
class _NotYetPorted extends StatelessWidget {
  const _NotYetPorted({required this.surface});

  final Surface surface;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(surface.title, style: context.titleStyle),
        const SizedBox(height: 4),
        Text(
          'This surface has not been ported to the new interface yet. '
          'Nothing about it has changed on this machine.',
          style: context.proseStyle,
        ),
      ],
    );
  }
}
