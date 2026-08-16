/// The self-hosted display coordinator window.
///
/// Facts are the daemon's [DisplayFacts]. Local state is draft form input only;
/// approvals, assignments, and revocations all cross as [ActionRequest]s and
/// return through the ordinary authoritative refresh.
library;

import 'dart:convert';

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/material.dart'
    show MaterialApp, Scaffold, SelectableText, ThemeMode;
import 'package:flutter/services.dart' show Clipboard, ClipboardData;
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import '../surfaces/page.dart';
import '../surfaces/surfaces.dart' show pageMargin;
import 'host.dart';
import 'theme.dart';
import 'type.dart';
import 'window.dart';

const Size _opening = Size(860, 720);
const Size _minimum = Size(700, 600);

class DisplaysApp extends StatelessWidget {
  const DisplaysApp({super.key, required this.client});

  final Client client;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Displays',
      debugShowCheckedModeBanner: false,
      theme: astrolabeTheme(Brightness.light),
      darkTheme: astrolabeTheme(Brightness.dark),
      themeMode: ThemeMode.dark,
      home: ClientScope(
        client: client,
        child: Scaffold(
          body: AstrolabeWindowFrame.secondary(
            title: 'Displays',
            nativeTitle: 'Displays — Astrolabe',
            nativeKey: displaysWindowKey,
            size: _opening,
            minimumSize: _minimum,
            dark: true,
            body: const DisplaysPage(),
          ),
        ),
      ),
    );
  }
}

class DisplaysPage extends StatelessWidget {
  const DisplaysPage({super.key});

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final view = ClientScope.watch(context);
    final display = view.display;

    return Padding(
      padding: pageMargin(t),
      child: SurfaceScaffold(
        title: 'Displays',
        prose: 'Enroll receivers on this network and pin each one to an exact '
            'World surface in an Orbit.',
        trailing: Button(
          label: 'Refresh',
          size: ButtonSize.sm,
          variant: ButtonVariant.outline,
          onPressed: view.inFlight.contains(ActionKeys.refresh)
              ? null
              : () => ClientScope.of(context)
                  .dispatch(const ActionRequest.refresh()),
        ),
        child: display == null
            ? const _Loading()
            : ListView(
                children: [
                  _Coordinator(display: display),
                  t.gap.y(Space.xl3),
                  _SectionTitle(
                    label: 'PAIRING REQUESTS',
                    count: display.pendingPairings.length,
                  ),
                  t.gap.y(Space.sm),
                  if (display.pendingPairings.isEmpty)
                    const Empty(
                      said: 'No receiver is waiting for approval.',
                      next: 'Open Astrolabe setup on a TV to begin pairing.',
                    )
                  else
                    for (final pairing in display.pendingPairings) ...[
                      _PairingCard(pairing: pairing),
                      t.gap.y(Space.sm),
                    ],
                  t.gap.y(Space.xl3),
                  _SectionTitle(
                    label: 'RECEIVERS',
                    count: display.devices.length,
                  ),
                  t.gap.y(Space.sm),
                  if (display.devices.isEmpty)
                    const Empty(
                      said: 'No receiver is enrolled.',
                      next: 'Pairing is confirmed on both the TV and here.',
                    )
                  else
                    for (final receiver in display.devices) ...[
                      _ReceiverCard(
                        receiver: receiver,
                        assignment: _assignmentFor(display, receiver.device),
                        display: display,
                        orbits: view.orbits,
                      ),
                      t.gap.y(Space.sm),
                    ],
                ],
              ),
      ),
    );
  }
}

DisplayAssignmentRow? _assignmentFor(DisplayFacts display, String device) {
  for (final assignment in display.assignments.reversed) {
    if (assignment.device == device && assignment.revokedAtUnixMs == null) {
      return assignment;
    }
  }
  return null;
}

class _Loading extends StatelessWidget {
  const _Loading();

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const Skeleton(height: 112),
        t.gap.y(Space.xl3),
        const Skeleton(height: 152),
        t.gap.y(Space.sm),
        const Skeleton(height: 152),
      ],
    );
  }
}

class _Coordinator extends StatelessWidget {
  const _Coordinator({required this.display});

  final DisplayFacts display;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Card(
      variant: CardVariant.surfaceSubtle,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(display.label, style: context.headingStyle),
              ),
              Button(
                label: 'Copy setup',
                size: ButtonSize.sm,
                variant: ButtonVariant.outline,
                tooltip: 'Copy the pinned receiver bootstrap JSON.',
                onPressed: () => Clipboard.setData(
                  ClipboardData(text: _receiverBootstrap(display)),
                ),
              ),
              t.gap.x(Space.sm),
              const Badge(label: 'SELF-HOSTED'),
            ],
          ),
          t.gap.y(Space.sm),
          _Fact(label: 'LAN ORIGIN', value: display.origin),
          t.gap.y(Space.xs),
          _Fact(
            label: 'CERTIFICATE SHA-256',
            value: display.certificateSha256,
          ),
        ],
      ),
    );
  }
}

String _receiverBootstrap(DisplayFacts display) => jsonEncode({
      'protocol_major': 1,
      'trust': {
        'kind': 'pinned_certificate',
        'origin': display.origin,
        'sha256': display.certificateSha256,
      },
      'certificate_pem': display.certificatePem,
      'rendezvous': null,
    });

class _SectionTitle extends StatelessWidget {
  const _SectionTitle({required this.label, required this.count});

  final String label;
  final int count;

  @override
  Widget build(BuildContext context) {
    return Row(
      children: [
        Expanded(
          child: Text(
            label,
            style: context.factLabelStyle.copyWith(
              color: context.text.l900,
              fontWeight: FontWeight.w700,
            ),
          ),
        ),
        Text('$count', style: context.labelStyle),
      ],
    );
  }
}

class _PairingCard extends StatelessWidget {
  const _PairingCard({required this.pairing});

  final DisplayPairingRow pairing;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final view = ClientScope.watch(context);
    final approve = ActionKeys.approveDisplayPairing(pairing.pairing);
    final reject = ActionKeys.rejectDisplayPairing(pairing.pairing);
    final busy =
        view.inFlight.contains(approve) || view.inFlight.contains(reject);

    return Card(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  '${_platform(pairing.platform)} · ${pairing.build}',
                  style: context.headingStyle,
                ),
              ),
              const Badge(label: 'VERIFY ON TV'),
            ],
          ),
          t.gap.y(Space.md),
          Text(
            pairing.confirmationPhrase.join('  '),
            style: context.monoStyle.copyWith(
              color: context.brand.l800,
              fontWeight: FontWeight.w700,
            ),
          ),
          t.gap.y(Space.sm),
          _Fact(
            label: 'CERTIFICATE SHA-256',
            value: pairing.certificateSha256,
          ),
          t.gap.y(Space.lg),
          Row(
            mainAxisAlignment: MainAxisAlignment.end,
            children: [
              Button(
                label: 'Reject',
                size: ButtonSize.sm,
                variant: ButtonVariant.ghost,
                onPressed: busy
                    ? null
                    : () => ClientScope.of(context).dispatch(
                          ActionRequest.displayPairingReject(
                            pairing: pairing.pairing,
                          ),
                        ),
              ),
              t.gap.x(Space.sm),
              Button(
                label: 'Approve…',
                size: ButtonSize.sm,
                onPressed: busy ? null : () => _approve(context, pairing),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

class _ReceiverCard extends StatelessWidget {
  const _ReceiverCard({
    required this.receiver,
    required this.assignment,
    required this.display,
    required this.orbits,
  });

  final DisplayReceiverRow receiver;
  final DisplayAssignmentRow? assignment;
  final DisplayFacts display;
  final List<OrbitRow> orbits;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final view = ClientScope.watch(context);
    final revoked = receiver.revokedAtUnixMs != null;
    final health = receiver.health;
    final assigning = view.inFlight.contains(
      ActionKeys.assignDisplay(receiver.device),
    );
    final revokingDevice = view.inFlight.contains(
      ActionKeys.revokeDisplayDevice(receiver.device),
    );
    final revokingAssignment = assignment != null &&
        view.inFlight.contains(
          ActionKeys.revokeDisplayAssignment(assignment!.assignment),
        );

    return Card(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(receiver.label, style: context.headingStyle),
                    t.gap.y(Space.xxs),
                    Text(
                      '${_platform(receiver.platform)} · ${receiver.build}',
                      style: context.labelStyle,
                    ),
                  ],
                ),
              ),
              Badge(
                label: revoked
                    ? 'REVOKED'
                    : health == null
                        ? 'NOT YET REPORTED'
                        : health.connection.toUpperCase(),
              ),
            ],
          ),
          t.gap.y(Space.md),
          if (assignment == null)
            Text('Unassigned', style: context.bodyStyle)
          else ...[
            Text(
              '${assignment!.world} · ${assignment!.surface}',
              style: context.bodyStyle.copyWith(fontWeight: FontWeight.w600),
            ),
            t.gap.y(Space.xxs),
            Text(
              'Orbit ${_short(assignment!.orbit)} · program '
              '${_short(assignment!.program)}',
              style: context.monoStyle,
            ),
            if (assignment!.syncGroup != null) ...[
              t.gap.y(Space.xxs),
              Text(
                'Sync ${assignment!.syncGroup} · '
                '${_syncMode(assignment!.syncMode!)} · '
                '${assignment!.staticDelayMs >= 0 ? '+' : ''}'
                '${assignment!.staticDelayMs} ms',
                style: context.labelStyle,
              ),
            ],
          ],
          if (health != null) ...[
            t.gap.y(Space.sm),
            Text(
              '${_words(health.playback)} · item '
              '${_short(health.currentItem)} · ${health.elapsedMs} ms',
              style: context.labelStyle,
            ),
            if (assignment?.syncGroup != null) ...[
              t.gap.y(Space.xxs),
              Text(
                'Residual ${health.driftResidualMs} ms · '
                '${health.correctionEvents} corrections',
                style: context.labelStyle,
              ),
            ],
            if (health.lastError != 'none') ...[
              t.gap.y(Space.xxs),
              Text(
                'Receiver reports ${_words(health.lastError)}',
                style: context.labelStyle.copyWith(
                  color: context.status.error.l800,
                ),
              ),
            ],
          ],
          t.gap.y(Space.lg),
          Wrap(
            spacing: t.size.sm,
            runSpacing: t.size.xs,
            children: [
              if (!revoked)
                Button(
                  label: assignment == null ? 'Assign…' : 'Replace…',
                  size: ButtonSize.sm,
                  onPressed:
                      assigning || orbits.isEmpty || display.surfaces.isEmpty
                          ? null
                          : () => _assign(
                                context,
                                receiver,
                                display.surfaces,
                                orbits,
                              ),
                  tooltip: orbits.isEmpty
                      ? 'This identity has no Orbit to assign.'
                      : display.surfaces.isEmpty
                          ? 'This build declares no display surfaces.'
                          : null,
                ),
              if (assignment != null)
                Button(
                  label: 'Unassign',
                  size: ButtonSize.sm,
                  variant: ButtonVariant.ghost,
                  onPressed: revokingAssignment
                      ? null
                      : () => _revokeAssignment(context, assignment!),
                ),
              if (!revoked)
                Button(
                  label: 'Revoke receiver…',
                  size: ButtonSize.sm,
                  variant: ButtonVariant.destructive,
                  onPressed: revokingDevice
                      ? null
                      : () => _revokeReceiver(context, receiver),
                ),
            ],
          ),
        ],
      ),
    );
  }
}

class _Fact extends StatelessWidget {
  const _Fact({required this.label, required this.value});

  final String label;
  final String value;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        SizedBox(
          width: 168,
          child: Text(label, style: context.factLabelStyle),
        ),
        t.gap.x(Space.sm),
        Expanded(
          child: SelectableText(
            value,
            style: context.monoStyle,
          ),
        ),
      ],
    );
  }
}

Future<void> _approve(
  BuildContext context,
  DisplayPairingRow pairing,
) async {
  final label = TextEditingController(text: _platform(pairing.platform));
  final approved = await showAppDialog<bool>(
    context: context,
    builder: (ctx) => DialogContent(
      children: [
        const DialogHeader(
          title: DialogTitle('Approve this display?'),
          description: DialogDescription(
            'Continue only if the six words and certificate fingerprint '
            'exactly match the receiver screen.',
          ),
        ),
        Text(pairing.confirmationPhrase.join('  ')),
        Input(
          controller: label,
          label: 'Display name',
          autofocus: true,
        ),
        DialogFooter(
          children: [
            Button(
              label: 'Cancel',
              variant: ButtonVariant.outline,
              onPressed: () => Navigator.pop(ctx, false),
            ),
            Button(
              label: 'Approve',
              onPressed: () => Navigator.pop(ctx, true),
            ),
          ],
        ),
      ],
    ),
  );
  final name = label.text.trim();
  label.dispose();
  if (approved != true || name.isEmpty || !context.mounted) return;
  ClientScope.of(context).dispatch(
    ActionRequest.displayPairingApprove(
      pairing: pairing.pairing,
      label: name,
    ),
  );
}

Future<void> _assign(
  BuildContext context,
  DisplayReceiverRow receiver,
  List<DisplaySurfaceRow> surfaces,
  List<OrbitRow> orbits,
) async {
  var orbit = orbits.first.space;
  var chosen = surfaces.first;
  var theme = DisplayTheme.dark;
  var stale = DisplayStaleAction.keepWithNativeBanner;
  var syncMode = DisplaySyncMode.stayInSync;
  final input = TextEditingController();
  final staleSeconds = TextEditingController(text: '120');
  final syncGroup = TextEditingController();
  final staticDelay = TextEditingController(text: '0');
  var valid = false;

  final assigned = await showAppDialog<bool>(
    context: context,
    builder: (ctx) => StatefulBuilder(
      builder: (ctx, setLocal) {
        final signage = chosen.world == 'com.lait.signage' &&
            chosen.surface == 'signage.program';
        final group = syncGroup.text.trim();
        final delay = int.tryParse(staticDelay.text.trim());
        valid = input.text.trim().isNotEmpty &&
            (int.tryParse(staleSeconds.text.trim()) ?? 0) >= 31 &&
            (group.isEmpty || RegExp(r'^[a-z0-9_-]{1,64}$').hasMatch(group)) &&
            delay != null &&
            delay >= -60000 &&
            delay <= 60000;
        return DialogContent(
          children: [
            DialogHeader(
              title: DialogTitle('Assign ${receiver.label}'),
              description: const DialogDescription(
                'The daemon validates the package input, queries the exact '
                'Orbit, and pins the resulting receiver program.',
              ),
            ),
            Text('ORBIT', style: context.factLabelStyle),
            Select<String>(
              value: orbit,
              onValueChange: (value) {
                if (value != null) setLocal(() => orbit = value);
              },
              trigger: const SelectTrigger(
                child: SelectValue(placeholder: 'Choose an Orbit'),
              ),
              child: SelectContent(
                children: [
                  for (final row in orbits)
                    SelectItem(
                      value: row.space,
                      label: row.name,
                      child: Text(row.name),
                    ),
                ],
              ),
            ),
            Text('DISPLAY SURFACE', style: context.factLabelStyle),
            Select<String>(
              value: _surfaceKey(chosen),
              onValueChange: (value) {
                if (value == null) return;
                setLocal(() {
                  chosen = surfaces.firstWhere(
                    (surface) => _surfaceKey(surface) == value,
                  );
                  input.clear();
                });
              },
              trigger: const SelectTrigger(
                child: SelectValue(placeholder: 'Choose a surface'),
              ),
              child: SelectContent(
                children: [
                  for (final surface in surfaces)
                    SelectItem(
                      value: _surfaceKey(surface),
                      label: surface.title,
                      child: Text('${surface.title} · ${surface.world}'),
                    ),
                ],
              ),
            ),
            if (signage)
              Input(
                controller: input,
                label: 'Signage program body ID',
                mono: true,
                onChanged: (_) => setLocal(() {}),
              )
            else
              Textarea(
                controller: input,
                label: 'Package input JSON',
                minLines: 3,
                maxLines: 6,
                onChanged: (_) => setLocal(() {}),
              ),
            Row(
              children: [
                Expanded(
                  child: _Choice<DisplayTheme>(
                    label: 'THEME',
                    value: theme,
                    options: DisplayTheme.values,
                    name: _theme,
                    onChanged: (value) => setLocal(() => theme = value),
                  ),
                ),
                context.tokens.gap.x(Space.md),
                Expanded(
                  child: Input(
                    controller: staleSeconds,
                    label: 'Stale after (seconds)',
                    onChanged: (_) => setLocal(() {}),
                  ),
                ),
              ],
            ),
            _Choice<DisplayStaleAction>(
              label: 'WHEN STALE',
              value: stale,
              options: DisplayStaleAction.values,
              name: _staleAction,
              onChanged: (value) => setLocal(() => stale = value),
            ),
            Input(
              controller: syncGroup,
              label: 'Sync group (optional)',
              mono: true,
              onChanged: (_) => setLocal(() {}),
            ),
            Row(
              children: [
                Expanded(
                  child: _Choice<DisplaySyncMode>(
                    label: 'SYNC MODE',
                    value: syncMode,
                    options: DisplaySyncMode.values,
                    name: _syncMode,
                    onChanged: (value) => setLocal(() => syncMode = value),
                  ),
                ),
                context.tokens.gap.x(Space.md),
                Expanded(
                  child: Input(
                    controller: staticDelay,
                    label: 'Static delay (ms, + advances)',
                    onChanged: (_) => setLocal(() {}),
                  ),
                ),
              ],
            ),
            DialogFooter(
              children: [
                Button(
                  label: 'Cancel',
                  variant: ButtonVariant.outline,
                  onPressed: () => Navigator.pop(ctx, false),
                ),
                Button(
                  label: 'Assign',
                  onPressed: valid ? () => Navigator.pop(ctx, true) : null,
                ),
              ],
            ),
          ],
        );
      },
    ),
  );

  final value = input.text.trim();
  final seconds = int.tryParse(staleSeconds.text.trim());
  final group = syncGroup.text.trim();
  final delay = int.tryParse(staticDelay.text.trim());
  input.dispose();
  staleSeconds.dispose();
  syncGroup.dispose();
  staticDelay.dispose();
  if (assigned != true ||
      value.isEmpty ||
      seconds == null ||
      delay == null ||
      !context.mounted) {
    return;
  }
  final signage =
      chosen.world == 'com.lait.signage' && chosen.surface == 'signage.program';
  ClientScope.of(context).dispatch(
    ActionRequest.displayAssignmentPut(
      device: receiver.device,
      orbit: orbit,
      world: chosen.world,
      surface: chosen.surface,
      inputJson: signage ? jsonEncode({'program': value}) : value,
      theme: theme,
      staleAfterMs: seconds * 1000,
      onStale: stale,
      syncGroup: group.isEmpty ? null : group,
      syncMode: syncMode,
      staticDelayMs: delay,
    ),
  );
}

class _Choice<T> extends StatelessWidget {
  const _Choice({
    required this.label,
    required this.value,
    required this.options,
    required this.name,
    required this.onChanged,
  });

  final String label;
  final T value;
  final List<T> options;
  final String Function(T) name;
  final ValueChanged<T> onChanged;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(label, style: context.factLabelStyle),
        context.tokens.gap.y(Space.xs),
        Select<T>(
          value: value,
          onValueChange: (value) {
            if (value != null) onChanged(value);
          },
          trigger: const SelectTrigger(
            child: SelectValue(placeholder: 'Choose'),
          ),
          child: SelectContent(
            children: [
              for (final option in options)
                SelectItem(
                  value: option,
                  label: name(option),
                  child: Text(name(option)),
                ),
            ],
          ),
        ),
      ],
    );
  }
}

Future<void> _revokeAssignment(
  BuildContext context,
  DisplayAssignmentRow assignment,
) async {
  final confirmed = await _confirm(
    context,
    title: 'Unassign this display?',
    description:
        'The receiver will get an unassigned program on its next poll.',
    action: 'Unassign',
  );
  if (confirmed != true || !context.mounted) return;
  ClientScope.of(context).dispatch(
    ActionRequest.displayAssignmentRevoke(
      assignment: assignment.assignment,
    ),
  );
}

Future<void> _revokeReceiver(
  BuildContext context,
  DisplayReceiverRow receiver,
) async {
  final confirmed = await _confirm(
    context,
    title: 'Revoke ${receiver.label}?',
    description: 'Its proof key will stop working immediately. Reconnecting '
        'this receiver requires a new pairing ceremony.',
    action: 'Revoke receiver',
    destructive: true,
  );
  if (confirmed != true || !context.mounted) return;
  ClientScope.of(context).dispatch(
    ActionRequest.displayDeviceRevoke(device: receiver.device),
  );
}

Future<bool?> _confirm(
  BuildContext context, {
  required String title,
  required String description,
  required String action,
  bool destructive = false,
}) {
  return showAppDialog<bool>(
    context: context,
    builder: (ctx) => DialogContent(
      children: [
        DialogHeader(
          title: DialogTitle(title),
          description: DialogDescription(description),
        ),
        DialogFooter(
          children: [
            Button(
              label: 'Cancel',
              variant: ButtonVariant.outline,
              onPressed: () => Navigator.pop(ctx, false),
            ),
            Button(
              label: action,
              variant: destructive
                  ? ButtonVariant.destructive
                  : ButtonVariant.primary,
              onPressed: () => Navigator.pop(ctx, true),
            ),
          ],
        ),
      ],
    ),
  );
}

String _surfaceKey(DisplaySurfaceRow surface) =>
    '${surface.world}\u0000${surface.surface}';

String _platform(String value) => switch (value) {
      'android_tv' => 'Android TV',
      'fire_tv' => 'Fire TV',
      'apple_tv' => 'Apple TV',
      'roku' => 'Roku',
      'webos' => 'webOS',
      _ => _words(value),
    };

String _theme(DisplayTheme value) => switch (value) {
      DisplayTheme.light => 'Light',
      DisplayTheme.dark => 'Dark',
      DisplayTheme.highContrast => 'High contrast',
    };

String _staleAction(DisplayStaleAction value) => switch (value) {
      DisplayStaleAction.keepWithNativeBanner => 'Keep with native banner',
      DisplayStaleAction.blank => 'Blank',
    };

String _syncMode(DisplaySyncMode value) => switch (value) {
      DisplaySyncMode.stayInSync => 'Stay in sync',
      DisplaySyncMode.positional => 'Positional',
    };

String _words(String value) => value
    .split('_')
    .where((part) => part.isNotEmpty)
    .map((part) => '${part[0].toUpperCase()}${part.substring(1)}')
    .join(' ');

String _short(String value) =>
    value.length <= 12 ? value : '${value.substring(0, 12)}…';
