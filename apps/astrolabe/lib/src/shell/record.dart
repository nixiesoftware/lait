/// What happened, at the bottom of every page.
///
/// An action that succeeded and left no trace on screen is indistinguishable
/// from one that was never dispatched, and "did that do anything" is the
/// question a client with no record cannot answer. So both halves are drawn:
/// what failed, and what worked.
///
/// Failures come first and are never collapsed. A refusal is the single most
/// important thing this interface can be showing, and the interface that hides
/// it while looking busy is the one that wastes an afternoon.
///
/// **This is a gap in covalence.** An inline alert — tonal fill, status accent,
/// a title and an optional action — is an ordinary design-system component;
/// spaceui carries `Banner` and `InfoBanner`, and covalence has only the modal
/// `AlertDialog` and the transient `Toast`. Built here from primitives so the
/// port did not stall, and kept deliberately thin so that moving it upstream is
/// a delete rather than a rewrite.
library;

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import 'type.dart';

class RecordStrip extends StatelessWidget {
  const RecordStrip({super.key});

  @override
  Widget build(BuildContext context) {
    final view = ClientScope.watch(context);
    if (view.failures.isEmpty && view.notices.isEmpty) {
      return const SizedBox.shrink();
    }

    return Padding(
      padding: const EdgeInsets.only(top: 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        mainAxisSize: MainAxisSize.min,
        children: [
          for (final failure in view.failures.take(3))
            _Line(
              tone: context.status.error.l800,
              wash: context.status.error.l50,
              // Both halves: what was being done, and what came back. Either
              // one alone sends somebody to guess the other.
              text: '${failure.what}: ${failure.error}',
              // A refusal is not a retry. Saying which kind it was is the
              // difference between "try again" and "stop and read this".
              note: failure.retryable ? 'This one could be worth retrying.' : null,
            ),
          for (final notice in view.notices.take(2))
            _Line(
              tone: context.text.l800,
              wash: context.surface.l100,
              text: notice.said,
              // Where a browser was sent, when that is what happened: a window
              // that came up behind another one is otherwise a click with no
              // visible result.
              note: notice.launched,
            ),
        ],
      ),
    );
  }
}

class _Line extends StatelessWidget {
  const _Line({
    required this.tone,
    required this.wash,
    required this.text,
    this.note,
  });

  final Color tone;
  final Color wash;
  final String text;
  final String? note;

  @override
  Widget build(BuildContext context) {
    return Container(
      margin: const EdgeInsets.only(top: 4),
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      decoration: BoxDecoration(
        color: wash,
        borderRadius: BorderRadius.circular(6),
        border: Border(left: BorderSide(color: tone, width: 2)),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(text, style: context.bodyStyle.copyWith(color: tone)),
          if (note != null) ...[
            const SizedBox(height: 2),
            Text(note!, style: context.labelStyle),
          ],
        ],
      ),
    );
  }
}
