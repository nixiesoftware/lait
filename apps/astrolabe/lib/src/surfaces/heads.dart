/// The heads serving this identity: what is up, and where it answers.
library;

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import '../shell/type.dart';
import 'page.dart';

class HeadsSurface extends StatelessWidget {
  const HeadsSurface({super.key});

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);

    return SurfaceScaffold(
      title: 'Heads',
      prose: 'What is serving this identity, and where it answers.',
      trailing: Button(
        onPressed: view.inFlight.contains('head.start')
            ? null
            : () => client.dispatch(const ActionRequest.startHead()),
        label: 'Start a head',
        variant: ButtonVariant.secondary,
        size: ButtonSize.sm,
        tooltip: 'Bring up a browser head for this identity',
      ),
      child: view.heads.isEmpty
          ? const Empty(
              said: 'No head is running.',
              next: 'Opening a World starts one, or start one here.',
            )
          : ListView.separated(
              itemCount: view.heads.length,
              separatorBuilder: (_, __) => t.gap.y(Space.md),
              itemBuilder: (context, index) {
                final head = view.heads[index];
                final stopping = view.inFlight.contains('head.stop:${head.id}');
                return Card(
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Expanded(
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Row(
                              children: [
                                Text(head.id, style: context.headingStyle),
                                t.gap.x(Space.md),
                                Badge(label: head.kind),
                                if (!head.owned) ...[
                                  t.gap.x(Space.xs),
                                  const Badge(
                                    label: 'external',
                                    variant: BadgeVariant.outline,
                                  ),
                                ],
                              ],
                            ),
                            t.gap.y(Space.xs),
                            // The address without its credential. A head URL
                            // carries a run token, and a page that printed it
                            // would put a credential in every screenshot.
                            Text(
                              head.origin ?? 'no address announced',
                              style: context.monoStyle,
                            ),
                            t.gap.y(Space.xxs),
                            Text(
                              // A browser head serves every Orbit its identity
                              // has; an MCP head is authored against one. The
                              // absence of a binding is a fact, not a blank.
                              head.orbit == null
                                  ? 'Serves every Orbit this identity has'
                                  : 'Bound to ${head.orbit}',
                              style: context.labelStyle,
                            ),
                          ],
                        ),
                      ),
                      Button(
                        // Ownership again: this client stops what it started
                        // and nothing else.
                        onPressed: !head.owned || stopping
                            ? null
                            : () => client
                                .dispatch(ActionRequest.stopHead(id: head.id)),
                        label: 'Stop',
                        size: ButtonSize.sm,
                        variant: ButtonVariant.destructiveOutline,
                        tooltip: head.owned
                            ? 'Stop this head'
                            : 'This client did not start that head',
                      ),
                    ],
                  ),
                );
              },
            ),
    );
  }
}
