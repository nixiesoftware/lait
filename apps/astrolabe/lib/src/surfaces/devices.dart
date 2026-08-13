/// The devices this client manages, and what may be done to each.
///
/// ## Ownership is the safety boundary, and the interface draws it as one
///
/// A control that cannot be used is disabled, not offered-and-refused: a person
/// who learns the rule from an error message has already tried the thing. Which
/// controls those are is not decided here — the core answers `owned` and
/// `canForceStop`, and this file draws the answer. An interface that inferred
/// the rule would offer a control the core refuses, on exactly the machine
/// where the two disagreed.
///
/// ## Removal and deletion are separate operations
///
/// Forgetting a device leaves what it holds alone. Deleting its data does not,
/// and cannot be undone — so it is confirmed by typing the device's name, and
/// the confirmation is checked against the row it was opened for rather than
/// against whatever is selected when the dialog closes.
library;

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import '../shell/type.dart';
import 'page.dart';

class DevicesSurface extends StatelessWidget {
  const DevicesSurface({super.key});

  @override
  Widget build(BuildContext context) {
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    final owned = view.devices.where((device) => device.owned).toList();

    return SurfaceScaffold(
      title: 'Devices',
      prose: 'The daemons this client runs, and the ones it only watches.',
      trailing: Button(
        onPressed: owned.isEmpty || view.inFlight.contains('device.stop-all')
            ? null
            : () => client.dispatch(const ActionRequest.stopAllOwned()),
        label: 'Stop everything owned',
        variant: ButtonVariant.destructiveOutline,
        size: ButtonSize.sm,
        // Offered only when there is something to stop. A control that is
        // always live and usually a no-op teaches a person to ignore it.
        tooltip: owned.isEmpty
            ? 'This client is not running any daemon'
            : 'Stop the ${owned.length} daemon(s) this client started',
      ),
      child: view.devices.isEmpty
          ? const Empty(
              said: 'No devices are registered.',
              next: 'A device is registered the first time this client starts one.',
            )
          : ListView.separated(
              itemCount: view.devices.length,
              separatorBuilder: (_, __) => const SizedBox(height: 8),
              itemBuilder: (context, index) =>
                  _DeviceCard(device: view.devices[index]),
            ),
    );
  }
}

class _DeviceCard extends StatelessWidget {
  const _DeviceCard({required this.device});

  final DeviceRow device;

  @override
  Widget build(BuildContext context) {
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    bool busy(String key) => view.inFlight.contains(key);

    return Card(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(
                      children: [
                        Text(device.label, style: context.headingStyle),
                        const SizedBox(width: 8),
                        Badge(
                          label: device.state,
                          variant: switch (device.state) {
                            'running' => BadgeVariant.success,
                            'stopped' => BadgeVariant.muted,
                            _ => BadgeVariant.outline,
                          },
                        ),
                        if (!device.owned) ...[
                          const SizedBox(width: 4),
                          // Said plainly rather than implied by a missing
                          // button: this client did not start it and will not
                          // stop it.
                          const Badge(label: 'external', variant: BadgeVariant.outline),
                        ],
                      ],
                    ),
                    const SizedBox(height: 2),
                    Text(device.home, style: context.monoStyle),
                  ],
                ),
              ),
              Wrap(
                spacing: 6,
                children: [
                  Button(
                    onPressed: device.state == 'running' ||
                            busy(ActionKeys.startDevice(device.id))
                        ? null
                        : () => client
                            .dispatch(ActionRequest.startDevice(id: device.id)),
                    label: 'Start',
                    size: ButtonSize.sm,
                    variant: ButtonVariant.secondary,
                  ),
                  Button(
                    onPressed: device.state != 'running' ||
                            busy(ActionKeys.stopDevice(device.id))
                        ? null
                        : () => client
                            .dispatch(ActionRequest.stopDevice(id: device.id)),
                    label: 'Stop',
                    size: ButtonSize.sm,
                    variant: ButtonVariant.secondary,
                  ),
                  Button(
                    onPressed: busy(ActionKeys.restartDevice(device.id))
                        ? null
                        : () => client.dispatch(
                            ActionRequest.restartDevice(id: device.id)),
                    label: 'Restart',
                    size: ButtonSize.sm,
                    variant: ButtonVariant.secondary,
                  ),
                  Button(
                    // Ownership, answered by the core. There is no pid-based
                    // path to this control and no way to reach it for a daemon
                    // this client did not spawn.
                    onPressed: !device.canForceStop
                        ? null
                        : () => client.dispatch(
                            ActionRequest.forceStopDevice(id: device.id)),
                    label: 'Force stop',
                    size: ButtonSize.sm,
                    variant: ButtonVariant.destructiveGhost,
                    tooltip: device.canForceStop
                        ? 'Kill the process this client started'
                        : device.owned
                            ? 'This build cannot force-stop a process'
                            : 'This client did not start that daemon',
                  ),
                ],
              ),
            ],
          ),
          if (device.degraded != null) ...[
            const SizedBox(height: 8),
            // A sampling failure degrades and preserves the last good reading.
            // It is never drawn as "nothing there", because those are different
            // facts and only one of them means the peer is gone.
            Text(
              'Observation degraded — ${device.degraded}. '
              'The figures above are the last good reading.',
              style: context.labelStyle
                  .copyWith(color: context.status.warning.l800),
            ),
          ],
          if (device.lastError != null) ...[
            const SizedBox(height: 4),
            Text(
              device.lastError!,
              style: context.labelStyle.copyWith(color: context.status.error.l800),
            ),
          ],
        ],
      ),
    );
  }
}
