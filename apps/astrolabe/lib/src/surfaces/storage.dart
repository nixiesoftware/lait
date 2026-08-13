/// What each Space is holding.
///
/// Every figure on this page is optional, and that is the contract rather than
/// an inconvenience. A footprint nobody could measure is **absent, never zero**:
/// an Orbit reported as holding 0 bytes is a claim, and one nobody asked is not.
/// So an unmeasured row says which kind of unmeasured it is — not up, or could
/// not be asked — and never draws a number to fill the column.
library;

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import '../shell/type.dart';
import 'page.dart';

class StorageSurface extends StatelessWidget {
  const StorageSurface({super.key});

  @override
  Widget build(BuildContext context) {
    final view = ClientScope.watch(context);

    return SurfaceScaffold(
      title: 'Storage',
      prose: 'What each Space is holding on this device.',
      child: view.storage.isEmpty
          ? const Empty(
              said: 'Nothing has been measured.',
              next: 'A Space is measured when it is up and can be asked.',
            )
          : ListView.separated(
              itemCount: view.storage.length,
              separatorBuilder: (_, __) => const SizedBox(height: 8),
              itemBuilder: (context, index) {
                final row = view.storage[index];
                return Card(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(row.name ?? row.orbit, style: context.headingStyle),
                      const SizedBox(height: 2),
                      Text(row.orbit, style: context.monoStyle),
                      const SizedBox(height: 12),
                      if (row.missing != null)
                        Text(
                          switch (row.missing!) {
                            // Two absences, said as two. Folding them together
                            // is the false-disconnection defect one layer down.
                            Missing.notPlaced =>
                              'Not measured — this Space is not running. '
                                  'Measuring will not start it.',
                            Missing.unreachable =>
                              'Not measured — this Space could not be asked.',
                          },
                          style: context.labelStyle
                              .copyWith(color: context.status.warning.l800),
                        )
                      else
                        Wrap(
                          spacing: 32,
                          runSpacing: 12,
                          children: [
                            _Figure(
                              label: 'ON DISK',
                              value: _bytes(row.bytesOnDisk),
                            ),
                            _Figure(
                              label: 'OBJECTS',
                              value: row.objectCount?.toString() ?? 'not measured',
                            ),
                            _Figure(
                              label: 'LAST VERIFIED',
                              value: _stamp(row.lastVerifiedMs),
                            ),
                          ],
                        ),
                    ],
                  ),
                );
              },
            ),
    );
  }
}

class _Figure extends StatelessWidget {
  const _Figure({required this.label, required this.value});

  final String label;
  final String value;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(label, style: context.factLabelStyle),
        const SizedBox(height: 2),
        Text(value, style: context.bodyStyle),
      ],
    );
  }
}

/// A size at the scale a person reads, or the absence spelled out.
String _bytes(BigInt? bytes) {
  if (bytes == null) return 'not measured';
  var value = bytes.toDouble();
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  var unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return unit == 0
      ? '${value.toStringAsFixed(0)} ${units[unit]}'
      : '${value.toStringAsFixed(1)} ${units[unit]}';
}

String _stamp(BigInt? millis) {
  if (millis == null) return 'never';
  final at = DateTime.fromMillisecondsSinceEpoch(millis.toInt());
  final ago = DateTime.now().difference(at);
  if (ago.isNegative) return 'in the future';
  if (ago.inMinutes < 1) return 'just now';
  if (ago.inHours < 1) return '${ago.inMinutes} min ago';
  if (ago.inDays < 1) return '${ago.inHours} h ago';
  return '${ago.inDays} d ago';
}
