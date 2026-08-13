/// The address-book window.
///
/// CLIENT-22 will draw the book here. This file is the host that window
/// needs first: it attaches to the one Rust core, draws the same chrome,
/// and closes a window rather than a peer. The live facts it shows are
/// how both windows prove they share one model.
library;

import 'package:covalence/covalence.dart' hide Surface;
import 'package:flutter/material.dart' show MaterialApp, Scaffold, ThemeMode;
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import 'theme.dart';
import 'type.dart';
import 'window.dart';

class BookApp extends StatefulWidget {
  const BookApp({super.key, required this.client});

  final Client client;

  @override
  State<BookApp> createState() => _BookAppState();
}

class _BookAppState extends State<BookApp> {
  ThemeMode _themeMode = ThemeMode.dark;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Address book',
      debugShowCheckedModeBanner: false,
      theme: astrolabeTheme(Brightness.light),
      darkTheme: astrolabeTheme(Brightness.dark),
      themeMode: _themeMode,
      home: ClientScope(
        client: widget.client,
        child: Scaffold(
          body: AstrolabeWindowFrame(
            title: 'Address book',
            closePolicy: AstrolabeWindowClosePolicy.close,
            chrome: const NativeWindowChrome(),
            body: BookPage(
              themeMode: _themeMode,
              onToggleTheme: () => setState(() {
                _themeMode = _themeMode == ThemeMode.dark
                    ? ThemeMode.light
                    : ThemeMode.dark;
              }),
            ),
          ),
        ),
      ),
    );
  }
}

/// The book's body until CLIENT-22 draws Cards here.
///
/// A canned-client test presses Refresh and reads the [ActionRequest].
/// The live facts (loading, library count, in-flight) are how a person
/// sees that this window is the same model as the main one.
class BookPage extends StatelessWidget {
  const BookPage({
    super.key,
    required this.themeMode,
    required this.onToggleTheme,
  });

  final ThemeMode themeMode;
  final VoidCallback onToggleTheme;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final client = ClientScope.of(context);
    final view = ClientScope.watch(context);
    final rereading = view.inFlight.contains(ActionKeys.refresh);
    final library = view.library_;

    return ListView(
      padding: t.padding.all(Space.xl6),
      children: [
        Text('Address book', style: context.titleStyle),
        t.gap.y(Space.sm),
        Text(
          'This window attaches to the same core as the Library. '
          'Cards will live here. Until they do, a refresh in either '
          'window disables the control in both.',
          style: context.proseStyle,
        ),
        t.gap.y(Space.xl6),
        Text(
          view.loading
              ? 'The core has not been read yet.'
              : library == null
                  ? 'No library has been read.'
                  : library.isEmpty
                      ? 'This identity serves no Worlds.'
                      : '${library.length} World${library.length == 1 ? '' : 's'} in the Library.',
          style: context.bodyStyle,
        ),
        t.gap.y(Space.xl3),
        Row(
          children: [
            Button(
              onPressed: rereading
                  ? null
                  : () => client.dispatch(const ActionRequest.refresh()),
              icon: AppIcons.refresh,
              label: 'Refresh',
              semanticLabel: 'Refresh',
              isLoading: rereading,
              tooltip: 'Read this machine again',
            ),
            t.gap.x(Space.sm),
            Button(
              onPressed: onToggleTheme,
              icon: themeMode == ThemeMode.dark
                  ? AppIcons.toggleOn
                  : AppIcons.toggleOff,
              label: themeMode == ThemeMode.dark ? 'Light' : 'Dark',
              variant: ButtonVariant.ghost,
            ),
          ],
        ),
      ],
    );
  }
}
