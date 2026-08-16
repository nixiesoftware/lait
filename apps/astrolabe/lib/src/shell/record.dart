/// Persistent operational truth at the bottom of every Astrolabe surface.
///
/// This is intentionally a status bar rather than a notification stack. A
/// refusal must remain visible, but three copies of the same successful open
/// are not three useful rows of UI. The core remains the authority; this bar
/// only chooses the most important sentence from the immutable view it sent.
library;

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import 'host.dart';
import 'type.dart';

const double kOperationalBarHeight = 32;

class OperationalBar extends StatelessWidget {
  const OperationalBar({super.key});

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final view = ClientScope.watch(context);
    final status = _identityStatus(context, view);
    final activity = _activity(view);

    return Semantics(
      container: true,
      liveRegion: view.inFlight.isNotEmpty || view.failures.isNotEmpty,
      label: '${status.label}. $activity',
      child: t.box.height(
        TokenEscape.rawSize(kOperationalBarHeight),
        child: Container(
          padding: t.padding.symmetric(h: Space.xl3),
          decoration: BoxDecoration(
            color: context.surface.l100,
            border: t.stroke.edge(top: context.border.l500),
          ),
          child: LayoutBuilder(
            builder: (context, constraints) {
              final showVersion = constraints.maxWidth >= 760;
              final showSpaces = constraints.maxWidth >= 620;
              return Row(
                children: [
                  if (view.loading)
                    Progress.spinner(
                      size: ProgressSize.xs,
                      color: status.tone,
                    )
                  else
                    Icon(status.icon, size: 14, color: status.tone),
                  t.gap.x(Space.sm),
                  Text(
                    status.label,
                    style: context.labelStyle.copyWith(
                      color: status.tone,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                  t.gap.x(Space.xl3),
                  Container(
                    width: t.stroke.xxs,
                    height: t.size.xl3,
                    color: context.border.l500,
                  ),
                  t.gap.x(Space.xl3),
                  Expanded(
                    child: Text(
                      activity,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: context.labelStyle,
                    ),
                  ),
                  t.gap.x(Space.xl3),
                  Text(
                    '${view.heads.length} ${_plural(view.heads.length, 'head')}',
                    style: context.labelStyle,
                  ),
                  if (showSpaces) ...[
                    t.gap.x(Space.xl3),
                    Text(
                      '${view.host?.orbitCount ?? view.orbits.length} '
                      '${_plural(view.host?.orbitCount ?? view.orbits.length, 'Space')}',
                      style: context.labelStyle,
                    ),
                  ],
                  if (showVersion && view.host != null) ...[
                    t.gap.x(Space.xl3),
                    Text('v${view.host!.version}', style: context.monoStyle),
                  ],
                  t.gap.x(Space.xl3),
                  Container(
                    width: t.stroke.xxs,
                    height: t.size.xl3,
                    color: context.border.l500,
                  ),
                  t.gap.x(Space.sm),
                  // The one act on a bar of facts, and it sits with them for
                  // the same reason they do: the book is this device's own,
                  // like the identity and the counts beside it. The rule is
                  // the boundary — what is true to its left, what can be done
                  // to its right.
                  Button(
                    onPressed: summonDisplays,
                    icon: AppIcons.cable,
                    semanticLabel: 'Displays',
                    variant: ButtonVariant.ghost,
                    size: ButtonSize.iconSm,
                    tooltip: 'Coordinate displays',
                  ),
                  t.gap.x(Space.xxs),
                  Button(
                    onPressed: summonBook,
                    icon: AppIcons.person,
                    semanticLabel: 'Address book',
                    variant: ButtonVariant.ghost,
                    size: ButtonSize.iconSm,
                    tooltip: 'Open the address book',
                  ),
                ],
              );
            },
          ),
        ),
      ),
    );
  }
}

/// Kept as a compatibility name for callers that still describe the old
/// stack. Its rendering is the persistent operational bar now.
class RecordStrip extends StatelessWidget {
  const RecordStrip({super.key});

  @override
  Widget build(BuildContext context) => const OperationalBar();
}

({String label, Color tone, IconData icon}) _identityStatus(
  BuildContext context,
  ClientView view,
) {
  if (view.loading) {
    return (
      label: 'Connecting to local identity',
      tone: context.text.l800,
      icon: AppIcons.refresh,
    );
  }
  if (view.failures.isNotEmpty) {
    return (
      label: 'Needs attention',
      tone: context.status.error.l800,
      icon: AppIcons.warningAmber,
    );
  }
  if (view.stale != null ||
      view.devices.any((device) => device.degraded != null)) {
    return (
      label: 'Local identity degraded',
      tone: context.status.warning.l800,
      icon: AppIcons.warningAmber,
    );
  }
  if (view.host == null) {
    return (
      label: 'Local identity unavailable',
      tone: context.status.warning.l800,
      icon: AppIcons.cable,
    );
  }
  return (
    label: 'Local identity online',
    tone: context.status.success.l800,
    icon: AppIcons.shieldCheck,
  );
}

String _activity(ClientView view) {
  if (view.inFlight.isNotEmpty) return _describeAction(view.inFlight.first);

  if (view.failures.isNotEmpty) {
    final failure = view.failures.first;
    return '${failure.what}: ${failure.error}';
  }

  // A record may be repeated by older cores. The bar is a current summary, so
  // identical sentences collapse here instead of taking over the window.
  final notices = view.notices.map(_noticeSummary).toSet();
  if (notices.isNotEmpty) return notices.first;
  return 'All local systems current';
}

String _noticeSummary(NoticeRow notice) {
  final launched = notice.launched;
  if (launched == null) return notice.said;

  // Launch tickets are single-use credentials. The core records the launch so
  // a person can tell that their click worked, but chrome only needs the safe
  // destination — never its query or fragment.
  final uri = Uri.tryParse(launched);
  if (uri == null || !uri.hasAuthority) return 'Opened World in browser';
  final port = uri.hasPort ? ':${uri.port}' : '';
  return 'Opened ${uri.scheme}://${uri.host}$port${uri.path}';
}

String _describeAction(String key) {
  if (key == ActionKeys.refresh) return 'Reading local state…';
  if (key.startsWith('open:')) return 'Starting World…';
  if (key.startsWith('head.')) return 'Updating head…';
  if (key.startsWith('device.')) return 'Updating device…';
  if (key.startsWith('space.')) return 'Reading Space…';
  return 'Working…';
}

String _plural(int count, String word) => count == 1 ? word : '${word}s';
