/// The Space plane: who is in the Space being administered, and this actor's
/// standing in it.
///
/// Standing is per Orbit rather than per identity — one identity may hold very
/// different standing in two Spaces, and a single answer would have to pick one
/// and be wrong about the other. So this page says nothing at all until a Space
/// has been chosen on Spaces, because until then there is no question to answer.
library;

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import '../shell/type.dart';
import 'page.dart';

class MembersSurface extends StatelessWidget {
  const MembersSurface({super.key});

  @override
  Widget build(BuildContext context) {
    final view = ClientScope.watch(context);
    final space = view.space;

    return SurfaceScaffold(
      title: 'Members',
      prose: 'Who is in the Space you are administering, and what you are in it.',
      child: space == null
          ? const Empty(
              said: 'No Space is being administered.',
              next: 'Choose one on Spaces — reading it is the act, so it is '
                  'not done on your behalf.',
            )
          : ListView(
              children: [
                Card(
                  variant: CardVariant.muted,
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(space.space, style: context.monoStyle),
                      const SizedBox(height: 8),
                      Row(
                        children: [
                          Text(
                            // `None` before admission — a fresh joiner whose
                            // inception has not landed has no actor id yet, and
                            // that is a fact rather than a blank.
                            space.whoami ?? 'no actor id yet',
                            style: context.bodyStyle,
                          ),
                          const SizedBox(width: 8),
                          Badge(
                            label: space.admin ? 'admin' : 'member',
                            variant: space.admin
                                ? BadgeVariant.solid
                                : BadgeVariant.muted,
                          ),
                        ],
                      ),
                    ],
                  ),
                ),
                const SizedBox(height: 16),
                Text('MEMBERS', style: context.factLabelStyle),
                const SizedBox(height: 8),
                if (space.members.isEmpty)
                  const Empty(said: 'This Space lists no members.')
                else
                  for (final member in space.members)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 8),
                      child: Card(
                        child: Row(
                          children: [
                            Expanded(
                              child: Column(
                                crossAxisAlignment: CrossAxisAlignment.start,
                                children: [
                                  Text(
                                    member.nick ?? member.id,
                                    style: context.bodyStyle,
                                  ),
                                  if (member.nick != null) ...[
                                    const SizedBox(height: 2),
                                    Text(member.id, style: context.monoStyle),
                                  ],
                                ],
                              ),
                            ),
                            if (member.admin)
                              const Badge(
                                label: 'admin',
                                variant: BadgeVariant.muted,
                              ),
                          ],
                        ),
                      ),
                    ),
                const SizedBox(height: 16),
                Text('THIS ACTOR\'S DEVICES', style: context.factLabelStyle),
                const SizedBox(height: 8),
                if (space.devices.isEmpty)
                  const Empty(said: 'No devices are bound to this actor here.')
                else
                  for (final device in space.devices)
                    Padding(
                      padding: const EdgeInsets.only(bottom: 4),
                      // Kept verbatim: the plane answers these as human text,
                      // and a client that assumed every line was an id would
                      // offer to revoke a sentence.
                      child: Text(device, style: context.monoStyle),
                    ),
              ],
            ),
    );
  }
}
