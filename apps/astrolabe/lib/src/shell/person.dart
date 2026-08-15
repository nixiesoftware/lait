/// The canonical person row — how an identity appears in any list, shared by
/// the book and every glance that references people. One anatomy: the face,
/// the name with the AI mark when the identity is an agent, and beneath them
/// a line that is only ever a fact — measured presence when a Space that
/// names them answered, else an authored note, else nothing.
///
/// Liveness reads through weight: an offline face dims hardest, an away face
/// part-way, and text dims through color instead of opacity so it never
/// drops below legibility. A surface composes gestures and badges around
/// this tile; the tile itself draws facts and nothing else.
library;

import 'package:covalence/covalence.dart' hide Image, Surface;
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import 'face.dart';
import 'type.dart';

class PersonTile extends StatelessWidget {
  const PersonTile({
    super.key,
    required this.name,
    required this.picture,
    required this.presence,
    this.agent = false,
    this.note,
    this.size = 40,
    this.trailing,
  });

  final String name;
  final String? picture;

  /// Measured presence, or `null` — the "could not be asked" absence, which
  /// draws nothing rather than a default.
  final PresenceView? presence;

  /// Wears the shipped AI mark beside the name. Kind is a mark, never a
  /// grouping — what an identity is and whether it is here are different
  /// axes.
  final bool agent;

  /// The authored note, drawn only when presence was not measured.
  final String? note;

  final double size;

  /// A badge or control the surface owns, placed at the row's end.
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final authored = note;
    final status = presenceLabel(presence) ??
        ((authored != null && authored.isNotEmpty) ? authored : null);
    final offline = presence == PresenceView.offline;
    final away = presence == PresenceView.away;
    return Row(
      children: [
        Opacity(
          opacity: offline ? 0.45 : (away ? 0.7 : 1.0),
          child: FacePlate(picture: picture, name: name, size: size),
        ),
        t.gap.x(Space.md),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              Row(
                children: [
                  Flexible(
                    child: Text(
                      name,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: offline
                          ? context.headingStyle.copyWith(
                              color: context.text.l700,
                            )
                          : context.headingStyle,
                    ),
                  ),
                  if (agent) ...[
                    t.gap.x(Space.xs),
                    const AiMark(),
                  ],
                ],
              ),
              if (status != null) ...[
                t.gap.y(Space.xxs),
                Text(
                  status,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: presence == PresenceView.online
                      ? context.labelStyle.copyWith(
                          color: context.status.success.l800,
                        )
                      : context.labelStyle,
                ),
              ],
            ],
          ),
        ),
        if (trailing != null) trailing!,
      ],
    );
  }
}

/// Wording for a measured presence, and nothing for an absence: an
/// unmeasured presence has no words because it is not a fact about the peer.
String? presenceLabel(PresenceView? presence) => switch (presence) {
      PresenceView.online => 'Online',
      PresenceView.away => 'Away',
      PresenceView.offline => 'Offline',
      null => null,
    };
