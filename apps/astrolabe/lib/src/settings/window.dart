/// A World-settings window that is deliberately separate from the client.
///
/// It receives a read-only snapshot on the command line and never starts a
/// second Rust core. That keeps Astrolabe single-owner while letting settings
/// behave like desktop settings: independently movable, focusable and closed.
library;

import 'dart:convert';
import 'dart:io';

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/material.dart'
    show Brightness, MaterialApp, Scaffold, ThemeMode;
import 'package:flutter/widgets.dart';

import '../shell/theme.dart';
import '../shell/type.dart';
import '../shell/window.dart';

const _settingsArgument = '--world-settings=';

@immutable
class WorldSettingsSnapshot {
  const WorldSettingsSnapshot({
    required this.key,
    required this.name,
    required this.orbit,
    required this.syncLabel,
    required this.syncDetail,
    required this.store,
    required this.worldMount,
    required this.entryPath,
    required this.version,
    required this.activeOrigin,
    required this.dark,
  });

  final String key;
  final String name;
  final String orbit;
  final String syncLabel;
  final String syncDetail;
  final String? store;
  final String worldMount;
  final String? entryPath;
  final int? version;
  final String? activeOrigin;
  final bool dark;

  String toArgument() {
    final json = jsonEncode({
      'key': key,
      'name': name,
      'orbit': orbit,
      'syncLabel': syncLabel,
      'syncDetail': syncDetail,
      'store': store,
      'worldMount': worldMount,
      'entryPath': entryPath,
      'version': version,
      'activeOrigin': activeOrigin,
      'dark': dark,
    });
    return '$_settingsArgument${base64Url.encode(utf8.encode(json))}';
  }

  static WorldSettingsSnapshot? fromArguments(Iterable<String> arguments) {
    String? argument;
    for (final candidate in arguments) {
      if (candidate.startsWith(_settingsArgument)) {
        argument = candidate;
        break;
      }
    }
    if (argument == null) return null;
    try {
      final encoded = argument.substring(_settingsArgument.length);
      final value = jsonDecode(utf8.decode(base64Url.decode(encoded)));
      if (value is! Map<String, dynamic>) return null;
      return WorldSettingsSnapshot(
        key: value['key'] as String,
        name: value['name'] as String,
        orbit: value['orbit'] as String,
        syncLabel: value['syncLabel'] as String,
        syncDetail: value['syncDetail'] as String,
        store: value['store'] as String?,
        worldMount: value['worldMount'] as String,
        entryPath: value['entryPath'] as String?,
        version: value['version'] as int?,
        activeOrigin: value['activeOrigin'] as String?,
        dark: value['dark'] as bool,
      );
    } on FormatException {
      return null;
    } on TypeError {
      return null;
    }
  }
}

typedef OpenWorldSettings = Future<void> Function(
  WorldSettingsSnapshot snapshot,
);

class WorldSettingsScope extends InheritedWidget {
  const WorldSettingsScope({
    super.key,
    required this.onOpen,
    required super.child,
  });

  final OpenWorldSettings onOpen;

  static Future<void> open(
    BuildContext context,
    WorldSettingsSnapshot snapshot,
  ) {
    final scope =
        context.dependOnInheritedWidgetOfExactType<WorldSettingsScope>();
    return (scope?.onOpen ?? launchWorldSettings)(snapshot);
  }

  @override
  bool updateShouldNotify(WorldSettingsScope oldWidget) =>
      onOpen != oldWidget.onOpen;
}

Future<void> launchWorldSettings(WorldSettingsSnapshot snapshot) async {
  // An absolute executable path is important on Windows, where Process.start
  // does not use the supplied working directory to resolve an executable.
  await Process.start(
    Platform.resolvedExecutable,
    [snapshot.toArgument()],
    mode: ProcessStartMode.detached,
  );
}

class WorldSettingsApp extends StatelessWidget {
  const WorldSettingsApp({super.key, required this.snapshot});

  final WorldSettingsSnapshot snapshot;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: '${snapshot.name} settings',
      debugShowCheckedModeBanner: false,
      theme: astrolabeTheme(Brightness.light),
      darkTheme: astrolabeTheme(Brightness.dark),
      themeMode: snapshot.dark ? ThemeMode.dark : ThemeMode.light,
      home: Scaffold(
        body: AstrolabeWindowFrame(
          key: const ValueKey('world-settings-window-shell'),
          title: '${snapshot.name} settings',
          closePolicy: AstrolabeWindowClosePolicy.close,
          body: WorldSettingsPage(snapshot: snapshot),
        ),
      ),
    );
  }
}

class WorldSettingsPage extends StatelessWidget {
  const WorldSettingsPage({super.key, required this.snapshot});

  final WorldSettingsSnapshot snapshot;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return ListView(
      padding: t.padding.all(Space.xl6),
      children: [
        Text('${snapshot.name} settings', style: context.titleStyle),
        t.gap.y(Space.sm),
        Text(
          'Runtime and location details reported by this World.',
          style: context.proseStyle,
        ),
        t.gap.y(Space.xl6),
        _SettingsSection(
          title: 'APPLICATION',
          children: [
            _Setting(label: 'SYNC STATUS', value: snapshot.syncLabel),
            _Setting(
              label: 'SYNC DETAIL',
              value: snapshot.syncDetail,
              maxLines: 3,
            ),
            _Setting(
              label: 'IMPLEMENTATION VERSION',
              value: snapshot.version == null
                  ? 'Not reported'
                  : 'v${snapshot.version}',
            ),
          ],
        ),
        t.gap.y(Space.xl3),
        _SettingsSection(
          title: 'LOCATIONS',
          children: [
            _Setting(
              label: 'STORE',
              value: snapshot.store ?? 'Not reported',
              mono: true,
            ),
            _Setting(
              label: 'WORLD MOUNT',
              value: snapshot.worldMount.isEmpty
                  ? 'Space front door'
                  : snapshot.worldMount,
              mono: true,
            ),
            _Setting(
              label: 'ENTRY PATH',
              value: snapshot.entryPath ?? 'Not declared',
              mono: true,
            ),
          ],
        ),
        t.gap.y(Space.xl3),
        _SettingsSection(
          title: 'ACTIVE INSTANCE',
          children: [
            _Setting(label: 'ORBIT', value: snapshot.orbit, mono: true),
            _Setting(
              label: 'ORIGIN',
              value: snapshot.activeOrigin ?? 'Not reported',
              mono: true,
            ),
          ],
        ),
      ],
    );
  }
}

class _SettingsSection extends StatelessWidget {
  const _SettingsSection({required this.title, required this.children});

  final String title;
  final List<Widget> children;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Container(
      padding: t.padding.all(Space.xl3),
      decoration: BoxDecoration(
        color: context.surface.l50,
        border: Border.all(color: context.border.l500, width: t.stroke.xxs),
        borderRadius: t.radius.all(Space.md),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Text(title, style: context.factLabelStyle),
          for (final child in children) ...[
            t.gap.y(Space.xl3),
            child,
          ],
        ],
      ),
    );
  }
}

class _Setting extends StatelessWidget {
  const _Setting({
    required this.label,
    required this.value,
    this.mono = false,
    this.maxLines,
  });

  final String label;
  final String value;
  final bool mono;
  final int? maxLines;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(label, style: context.factLabelStyle),
        t.gap.y(Space.xxs),
        Text(
          value,
          maxLines: maxLines ?? (mono ? 2 : 1),
          overflow: TextOverflow.ellipsis,
          style: mono ? context.monoStyle : context.bodyStyle,
        ),
      ],
    );
  }
}
