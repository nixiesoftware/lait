/// The host plane: the Orbits this identity has, and where they live.
///
/// This is the surface that has to work when *no* Space exists yet, which is
/// precisely when no World head can draw a page to do it from. Founding a Space
/// and entering one from an invite are therefore this client's own flows and
/// not a World's — they are named at the bottom of this page and have not been
/// ported yet, which is said plainly rather than left as an absence somebody
/// has to discover.
library;

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import '../shell/type.dart';
import 'page.dart';

class SpacesSurface extends StatelessWidget {
  const SpacesSurface({super.key});

  @override
  Widget build(BuildContext context) {
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);

    return SurfaceScaffold(
      title: 'Spaces',
      prose: 'The Orbits this identity has, and where each one is stored.',
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (view.host != null) ...[
            Card(
              variant: CardVariant.muted,
              child: Wrap(
                spacing: 32,
                runSpacing: 12,
                children: [
                  _Fact(label: 'BUILD', value: view.host!.version),
                  _Fact(label: 'IDENTITY', value: view.host!.identityHome, mono: true),
                  _Fact(label: 'STORES', value: view.host!.spacesRoot, mono: true),
                ],
              ),
            ),
            const SizedBox(height: 16),
          ],
          Expanded(
            child: view.orbits.isEmpty
                ? const Empty(
                    said: 'This identity has no Orbits.',
                    next: 'Found a Space, or enter one from an invite — both '
                        'flows are still on the retiring interface.',
                  )
                : ListView.separated(
                    itemCount: view.orbits.length,
                    separatorBuilder: (_, __) => const SizedBox(height: 8),
                    itemBuilder: (context, index) {
                      final orbit = view.orbits[index];
                      final chosen = view.space?.space == orbit.space;
                      final reading =
                          view.inFlight.contains('space.read:${orbit.space}');
                      return Card(
                        selected: chosen,
                        child: Row(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Expanded(
                              child: Column(
                                crossAxisAlignment: CrossAxisAlignment.start,
                                children: [
                                  Text(
                                    orbit.name.isEmpty ? 'Unnamed Space' : orbit.name,
                                    style: context.headingStyle,
                                  ),
                                  const SizedBox(height: 2),
                                  Text(orbit.space, style: context.monoStyle),
                                  const SizedBox(height: 2),
                                  Text(orbit.path, style: context.labelStyle),
                                ],
                              ),
                            ),
                            Wrap(
                              spacing: 6,
                              children: [
                                Button(
                                  // Choosing a Space to administer *is* the
                                  // act: it costs a read. Listing above it
                                  // stays passive and places nothing.
                                  onPressed: reading
                                      ? null
                                      : () => client.dispatch(
                                          ActionRequest.readSpace(
                                              orbit: orbit.space)),
                                  label: chosen ? 'Re-read' : 'Administer',
                                  isLoading: reading,
                                  size: ButtonSize.sm,
                                  variant: ButtonVariant.secondary,
                                  tooltip:
                                      'Read this Space, and show it on Members',
                                ),
                                Button(
                                  onPressed: () => client.dispatch(
                                      ActionRequest.forgetOrbit(
                                          space: orbit.space)),
                                  label: 'Forget',
                                  size: ButtonSize.sm,
                                  variant: ButtonVariant.destructiveGhost,
                                  // Registry-only, and said so before the
                                  // click rather than after it.
                                  tooltip: 'Remove it from this registry. The '
                                      'store on disk is left alone.',
                                ),
                              ],
                            ),
                          ],
                        ),
                      );
                    },
                  ),
          ),
        ],
      ),
    );
  }
}

class _Fact extends StatelessWidget {
  const _Fact({required this.label, required this.value, this.mono = false});

  final String label;
  final String value;
  final bool mono;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(label, style: context.factLabelStyle),
        const SizedBox(height: 2),
        Text(value, style: mono ? context.monoStyle : context.bodyStyle),
      ],
    );
  }
}
