/// The face on a card — the canonical presentation of an identity's picture,
/// shared by every surface that shows a person. The book leads with it, and
/// the Library's at-a-glance panel draws the very same plate, so a face
/// resolves through the book everywhere rather than being re-derived
/// per-surface.
library;

import 'dart:convert' show base64Decode;
import 'dart:typed_data' show Uint8List;

import 'package:covalence/covalence.dart' hide Image, Surface;
import 'package:flutter/widgets.dart';

import 'type.dart';

/// The stored picture when one was authored, else the default — a monogram,
/// or the person mark when there is nothing to monogram. Boxed like the
/// reference client's plates, with the stroke painted in the FOREGROUND:
/// painted behind, a cover-fit picture simply hides it, and only the default
/// face appeared to have a border.
class FacePlate extends StatelessWidget {
  const FacePlate({
    super.key,
    required this.picture,
    required this.name,
    required this.size,
  });

  final String? picture;
  final String name;
  final double size;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final bytes = _pictureBytes(picture);
    return t.box.square(
      // reason: the face copies the reference client's plate — pinned like
      // the caption controls, it sits against type rather than the spacing
      // rhythm.
      TokenEscape.rawSize(size),
      child: DecoratedBox(
        position: DecorationPosition.foreground,
        decoration: BoxDecoration(
          border: Border.all(
            color: context.border.l500,
            width: t.stroke.xxs,
          ),
          borderRadius: t.radius.all(Space.xxs),
        ),
        child: DecoratedBox(
          decoration: BoxDecoration(
            color: context.surface.l200,
            borderRadius: t.radius.all(Space.xxs),
          ),
          child: bytes != null
              ? ClipRRect(
                  borderRadius: t.radius.all(Space.xxs),
                  child: Image.memory(
                    bytes,
                    fit: BoxFit.cover,
                    gaplessPlayback: true,
                  ),
                )
              : Center(
                  child: name.isEmpty
                      ? Icon(AppIcons.person, color: context.text.l700)
                      : Text(
                          name.substring(0, 1).toUpperCase(),
                          style: context.headingStyle,
                        ),
                ),
        ),
      ),
    );
  }
}

/// Decode the stored `<mime>;base64,<data>` form. The engine validated it at
/// write, so a miss here is a corrupt store answered with the default face —
/// never a crash in a list row.
Uint8List? _pictureBytes(String? stored) {
  if (stored == null) return null;
  final split = stored.indexOf(';base64,');
  if (split < 0) return null;
  try {
    return base64Decode(stored.substring(split + 8));
  } catch (_) {
    return null;
  }
}
