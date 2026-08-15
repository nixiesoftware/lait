/// The onboarding gates for the Space being administered.
///
/// A diagnosis that could not be taken is **never** "every gate passes". That
/// is why an absent diagnosis is drawn as an absence with its own sentence
/// rather than as an empty list of gates, which would read as a clean bill of
/// health from a machine nobody asked.
///
/// `Warn` is deliberately not blocking. A key-custody problem is urgent to fix
/// and irrelevant to whether somebody is onboarded, so it is drawn with its own
/// tone and is not what the blocker points at.
library;

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import '../shell/type.dart';
import 'page.dart';

class DiagnosticsSurface extends StatelessWidget {
  const DiagnosticsSurface({super.key});

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    final space = view.space;
    final diagnosis = space?.diagnosis;

    return SurfaceScaffold(
      title: 'Diagnostics',
      prose: 'What is between this device and a Space that works.',
      trailing: space == null
          ? null
          : Button(
              onPressed: view.inFlight.contains('space.read:${space.space}')
                  ? null
                  : () =>
                      client.dispatch(ActionRequest.readSpace(orbit: space.space)),
              label: 'Take it again',
              size: ButtonSize.sm,
              variant: ButtonVariant.secondary,
            ),
      child: switch ((space, diagnosis)) {
        (null, _) => const Empty(
            said: 'No Space is being administered.',
            next: 'Choose one on Spaces to see its gates.',
          ),
        (_, null) => const Empty(
            said: 'This Space could not be diagnosed.',
            next: 'That is not the same as every gate passing — nothing here '
                'has been checked.',
          ),
        (_, final taken) => ListView(
            children: [
              Card(
                variant: CardVariant.muted,
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(taken!.summary, style: context.bodyStyle),
                    if (taken.blockedOn != null) ...[
                      t.gap.y(Space.xs),
                      Text(
                        'Blocked on: ${taken.blockedOn}',
                        style: context.labelStyle
                            .copyWith(color: context.status.warning.l800),
                      ),
                    ],
                  ],
                ),
              ),
              t.gap.y(Space.xl3),
              for (final gate in taken.gates)
                Padding(
                  padding: t.padding.only(bottom: Space.md),
                  child: Card(
                    child: Row(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Badge(
                          label: switch (gate.state) {
                            GateState.pass => 'pass',
                            GateState.wait => 'waiting',
                            GateState.fail => 'fail',
                            GateState.warn => 'warn',
                            GateState.skip => 'skipped',
                          },
                          variant: switch (gate.state) {
                            GateState.pass => BadgeVariant.success,
                            GateState.fail => BadgeVariant.error,
                            GateState.warn => BadgeVariant.warning,
                            GateState.wait => BadgeVariant.outline,
                            GateState.skip => BadgeVariant.muted,
                          },
                        ),
                        t.gap.x(Space.xl),
                        Expanded(
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              Text(gate.label, style: context.bodyStyle),
                              t.gap.y(Space.xxs),
                              // The current value, or what it is waiting on.
                              Text(gate.detail, style: context.labelStyle),
                            ],
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
            ],
          ),
      },
    );
  }
}
