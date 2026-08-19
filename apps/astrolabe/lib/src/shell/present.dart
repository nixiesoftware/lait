/// Big Picture: this machine as a screen.
///
/// The *member* profile of REACH's two-profile split. Astrolabe already holds
/// the Space these pixels came from, so nothing here pairs, enrols, or carries
/// a credential — the surface asks the daemon to render one exact World surface
/// and draws what comes back. Leaving is always available, and revocation needs
/// no message: losing standing simply stops the answer.
///
/// The three states the requirement Spec keeps separate stay separate here.
/// *Source truth* is the program's own assessment, *delivery* is whether the
/// last re-ask answered, and an item this screen cannot draw refuses visibly
/// rather than being dropped or blanked.
library;

import 'dart:async';
import 'dart:convert';

import 'package:covalence/covalence.dart' hide Image, Surface;
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../core/client.dart';
import 'type.dart';
import 'window.dart';

/// How long an item with no declared duration holds the screen.
const Duration _untimedHold = Duration(seconds: 15);

/// The floor on re-asking, whatever a program declares. A surface that asked
/// for a one-millisecond refresh would otherwise spend the machine on it.
const Duration _refreshFloor = Duration(seconds: 5);

class BigPictureSurface extends StatefulWidget {
  const BigPictureSurface({
    super.key,
    required this.presentation,
    this.chrome = const ManagerWindowControlHost(),
  });

  final PresentationFacts presentation;

  /// How this surface takes and gives back the display. Injected so a widget
  /// test drives the real surface without a window under it.
  final WindowControlHost chrome;

  @override
  State<BigPictureSurface> createState() => _BigPictureSurfaceState();
}

class _BigPictureSurfaceState extends State<BigPictureSurface> {
  int _index = 0;
  Timer? _advance;
  Timer? _refresh;

  /// The last size this surface was laid out at.
  ///
  /// A television power-cycles weekly and a desktop monitor is unplugged
  /// mid-meeting; both come back as a resize. Re-asking on that change is what
  /// makes the render match the glass rather than the glass it was asked for.
  Size? _acquired;

  @override
  void initState() {
    super.initState();
    // Take the display, not the work area. A maximised window keeps its frame
    // and leaves the taskbar over it, which is a large window rather than a
    // screen — and the caption's own maximise control already means that.
    unawaited(widget.chrome.setFullScreen(true));
    _schedule();
  }

  @override
  void didUpdateWidget(BigPictureSurface old) {
    super.didUpdateWidget(old);
    final before = old.presentation.program;
    final now = widget.presentation.program;
    // A new revision restarts the program; a re-ask that returned the same
    // thing must not, or a long item would never finish.
    if (before != now) {
      _index = 0;
      _schedule();
    }
  }

  @override
  void dispose() {
    _advance?.cancel();
    _refresh?.cancel();
    // Give the display back on the way out, whatever the reason for leaving.
    // A client that exited Big Picture and kept the screen would be a window
    // nobody could get behind.
    unawaited(widget.chrome.setFullScreen(false));
    super.dispose();
  }

  List<PresentedItem> get _items =>
      widget.presentation.program?.items ?? const [];

  void _schedule() {
    _advance?.cancel();
    _refresh?.cancel();

    final program = widget.presentation.program;
    if (program == null) return;

    final refreshMs = program.refreshAfterMs;
    if (refreshMs != null) {
      final after = Duration(milliseconds: refreshMs);
      _refresh = Timer(
        after < _refreshFloor ? _refreshFloor : after,
        () => ClientScope.of(context)
            .dispatch(const ActionRequest.presentRefresh()),
      );
    }

    if (_items.length <= _index) return;
    final item = _items[_index];
    final hold = item.durationMs == null
        ? _untimedHold
        : Duration(milliseconds: item.durationMs!);
    _advance = Timer(hold, _next);
  }

  void _next() {
    if (!mounted) return;
    final last = _index >= _items.length - 1;
    if (!last) {
      setState(() => _index += 1);
      _schedule();
      return;
    }
    switch (widget.presentation.program?.cycle) {
      case 'loop':
        setState(() => _index = 0);
        _schedule();
      case 'poll_at_end':
        ClientScope.of(context)
            .dispatch(const ActionRequest.presentRefresh());
      case 'blank_at_end':
        setState(() => _index = _items.length);
      // hold_last, and anything a newer daemon might send: stay where we are
      // rather than guess. An unknown cycle holding the last frame is the
      // conservative reading; blanking on one would discard a program because
      // this build did not recognise a word.
      default:
        break;
    }
  }

  void _leave() =>
      ClientScope.of(context).dispatch(const ActionRequest.leavePresentation());

  @override
  Widget build(BuildContext context) {
    final presentation = widget.presentation;
    return Shortcuts(
      shortcuts: const <ShortcutActivator, Intent>{
        SingleActivator(LogicalKeyboardKey.escape): _Leave(),
      },
      child: Actions(
        actions: <Type, Action<Intent>>{
          _Leave: CallbackAction<_Leave>(onInvoke: (_) {
            _leave();
            return null;
          }),
        },
        child: Focus(
          autofocus: true,
          child: MouseRegion(
            // Hidden while showing, kept while choosing: a chooser you cannot
            // point at is not a chooser.
            cursor: presentation.chosen == null
                ? MouseCursor.defer
                : SystemMouseCursors.none,
            child: LayoutBuilder(
              builder: (context, constraints) {
                final size = constraints.biggest;
                if (_acquired != null && _acquired != size) {
                  // Deferred: dispatching during layout would re-enter the
                  // build that is measuring us.
                  WidgetsBinding.instance.addPostFrameCallback((_) {
                    if (mounted) {
                      ClientScope.of(context)
                          .dispatch(const ActionRequest.presentRefresh());
                    }
                  });
                }
                _acquired = size;
                return ColoredBox(
                  color: const Color(0xFF000000),
                  child: presentation.chosen == null
                      // Entered and not yet pointed at anything. A real state,
                      // and the one the person is in the instant they press
                      // the control.
                      ? const PresentationChooser()
                      : Stack(
                          fit: StackFit.expand,
                          children: [
                            _Scene(
                              item: _index < _items.length
                                  ? _items[_index]
                                  : null,
                              empty: presentation.program != null &&
                                  _items.isEmpty,
                            ),
                            _Chrome(
                              presentation: presentation,
                              onLeave: _leave,
                            ),
                          ],
                        ),
                );
              },
            ),
          ),
        ),
      ),
    );
  }
}

class _Leave extends Intent {
  const _Leave();
}

/// What the current item draws, or an honest statement of why it does not.
class _Scene extends StatelessWidget {
  const _Scene({required this.item, required this.empty});

  final PresentedItem? item;
  final bool empty;

  @override
  Widget build(BuildContext context) {
    final current = item;
    if (current == null) {
      return _Said(
        // The two absences are different facts and are drawn as such.
        headline: empty ? 'This program has no items' : 'Nothing to show',
        detail: empty
            ? 'The surface answered, and its program is empty.'
            : 'Waiting for the first render.',
      );
    }
    return switch (current.scene) {
      PresentedScene_Frame(:final bytes) => Image.memory(
          Uint8List.fromList(bytes),
          fit: BoxFit.contain,
          gaplessPlayback: true,
        ),
      PresentedScene_Blank(:final reason) => _Said(
          headline: switch (reason) {
            'source_unavailable' => 'This source is unavailable',
            'program_ended' => 'The program has ended',
            _ => 'Nothing to show',
          },
          detail: 'Blank was what the program asked for.',
        ),
      PresentedScene_Unsupported(:final output) => _Said(
          headline: 'This screen cannot draw $output',
          detail: 'Live media is served by a display coordinator to a paired '
              'receiver. Astrolabe as a screen draws frames.',
        ),
    };
  }
}

/// Text on the presentation ground.
///
/// Colours are stated rather than taken from the theme. This surface is always
/// black — it is a screen, not a page — so theme-derived text would be dark on
/// black under a light theme, which is the one case nobody would test and
/// everybody would meet.
class _Said extends StatelessWidget {
  const _Said({required this.headline, required this.detail});

  final String headline;
  final String detail;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    return Center(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(
            headline,
            style: context.headingStyle.copyWith(
              color: const Color(0xFFFFFFFF),
            ),
          ),
          t.gap.y(Space.sm),
          Text(
            detail,
            style: context.labelStyle.copyWith(
              color: const Color(0x99FFFFFF),
            ),
          ),
        ],
      ),
    );
  }
}

/// Receiver-native chrome, and the rule it follows.
///
/// Product pixels may not suppress trust, source-state or delivery-state
/// treatment. That rule was written for televisions and it is not about
/// televisions: a screen that could be made to look current while it is stale
/// is the false-assurance defect wherever it runs. So this draws over the
/// frame, and only when there is something to say.
class _Chrome extends StatelessWidget {
  const _Chrome({required this.presentation, required this.onLeave});

  final PresentationFacts presentation;
  final VoidCallback onLeave;

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final program = presentation.program;
    final failure = presentation.failure;
    final assessment = program?.assessment;
    final degraded = assessment != null && assessment != 'current';

    return Padding(
      padding: const EdgeInsets.all(24),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  presentation.chosen?.title ?? '',
                  style: context.labelStyle.copyWith(
                    color: const Color(0x99FFFFFF),
                  ),
                ),
              ),
              Text(
                'Esc to leave',
                style: context.labelStyle.copyWith(
                  color: const Color(0x66FFFFFF),
                ),
              ),
            ],
          ),
          const Spacer(),
          if (degraded) ...[
            _Banner(
              text: assessment == 'unavailable'
                  ? 'This source is unavailable. Showing what was last verified.'
                  : 'This source is partial: '
                      '${program!.partialReasons.join(', ')}.',
            ),
            t.gap.y(Space.xs),
          ],
          // Delivery is a separate state from source truth, and it says so:
          // this screen could not be re-asked, which is not the same as the
          // source having gone bad.
          if (failure != null)
            _Banner(text: 'Could not refresh: $failure'),
        ],
      ),
    );
  }
}

/// Point this screen at something, from inside the mode.
///
/// A surface rather than a dialog, and the difference is the interaction it
/// belongs to: entering Big Picture is one press, so the choosing that follows
/// happens on the screen the person just made. A modal in front of the control
/// would be asking what to show before there was anything to show it on.
///
/// It states its own preconditions rather than being suppressed by them. A
/// screen that cannot draw anything is a real state and can say which of the
/// three reasons it is in — which a disabled control in a caption bar could
/// never do at this distance.
class PresentationChooser extends StatefulWidget {
  const PresentationChooser({super.key});

  @override
  State<PresentationChooser> createState() => _PresentationChooserState();
}

class _PresentationChooserState extends State<PresentationChooser> {
  String? _orbit;
  DisplaySurfaceRow? _surface;
  final TextEditingController _input = TextEditingController();

  @override
  void dispose() {
    _input.dispose();
    super.dispose();
  }

  static bool _isSignage(DisplaySurfaceRow? row) =>
      row?.world == 'com.lait.signage' && row?.surface == 'signage.program';

  @override
  Widget build(BuildContext context) {
    final t = context.tokens;
    final view = ClientScope.watch(context);
    final display = view.display;
    final surfaces = display?.surfaces ?? const <DisplaySurfaceRow>[];

    // Each absence says which kind it is. A coordinator that has not answered
    // is a read that has not happened; one that answered with no surfaces is a
    // build that ships none. Collapsing them sends somebody hunting a missing
    // package when the daemon is simply not up.
    final blocked = display == null
        ? 'The display coordinator has not answered yet.'
        : surfaces.isEmpty
            ? 'This build bundles no display surface.'
            : view.orbits.isEmpty
                ? 'This identity has no Orbit to draw from.'
                : null;

    if (blocked != null) {
      return _Said(headline: 'Nothing to show here', detail: blocked);
    }

    final orbit = _orbit ?? view.orbits.first.space;
    final surface = _surface ?? surfaces.first;
    final ready = _input.text.trim().isNotEmpty;

    return Center(
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 560),
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text(
                'What should this screen show?',
                textAlign: TextAlign.center,
                style: context.headingStyle
                    .copyWith(color: const Color(0xFFFFFFFF)),
              ),
              t.gap.y(Space.xl3),
              _ChooserLabel('ORBIT'),
              Select<String>(
                value: orbit,
                onValueChange: (value) {
                  if (value != null) setState(() => _orbit = value);
                },
                trigger: const SelectTrigger(
                  child: SelectValue(placeholder: 'Choose an Orbit'),
                ),
                child: SelectContent(
                  children: [
                    for (final row in view.orbits)
                      SelectItem(
                        value: row.space,
                        label: row.name,
                        child: Text(row.name),
                      ),
                  ],
                ),
              ),
              t.gap.y(Space.md),
              _ChooserLabel('DISPLAY SURFACE'),
              Select<String>(
                value: '${surface.world}/${surface.surface}',
                onValueChange: (value) {
                  if (value == null) return;
                  setState(() {
                    _surface = surfaces.firstWhere(
                      (s) => '${s.world}/${s.surface}' == value,
                    );
                    _input.clear();
                  });
                },
                trigger: const SelectTrigger(
                  child: SelectValue(placeholder: 'Choose a surface'),
                ),
                child: SelectContent(
                  children: [
                    for (final row in surfaces)
                      SelectItem(
                        value: '${row.world}/${row.surface}',
                        label: row.title,
                        child: Text('${row.title} · ${row.world}'),
                      ),
                  ],
                ),
              ),
              t.gap.y(Space.md),
              if (_isSignage(surface))
                Input(
                  controller: _input,
                  label: 'Signage program body ID',
                  mono: true,
                  onChanged: (_) => setState(() {}),
                )
              else
                Textarea(
                  controller: _input,
                  label: 'Package input JSON',
                  minLines: 2,
                  maxLines: 5,
                  onChanged: (_) => setState(() {}),
                ),
              t.gap.y(Space.xl3),
              Button(
                label: 'Show it',
                onPressed: ready ? () => _show(orbit, surface) : null,
              ),
              t.gap.y(Space.sm),
              Text(
                'Esc to leave',
                textAlign: TextAlign.center,
                style:
                    context.labelStyle.copyWith(color: const Color(0x66FFFFFF)),
              ),
            ],
          ),
        ),
      ),
    );
  }

  void _show(String orbit, DisplaySurfaceRow surface) {
    ClientScope.of(context).dispatch(
      ActionRequest.presentHere(
        orbit: orbit,
        world: surface.world,
        surface: surface.surface,
        // A Signage program id is typed bare and wrapped here; every other
        // surface takes its package's own JSON verbatim. The daemon hands
        // whatever this is to the package's canonicalizer, which is the only
        // thing entitled to judge it.
        input: _isSignage(surface)
            ? jsonEncode({'program': _input.text.trim()})
            : _input.text.trim(),
        title: surface.title,
      ),
    );
  }
}

class _ChooserLabel extends StatelessWidget {
  const _ChooserLabel(this.text);

  final String text;

  @override
  Widget build(BuildContext context) => Text(
        text,
        style:
            context.factLabelStyle.copyWith(color: const Color(0x99FFFFFF)),
      );
}

class _Banner extends StatelessWidget {
  const _Banner({required this.text});

  final String text;

  @override
  Widget build(BuildContext context) {
    return DecoratedBox(
      decoration: const BoxDecoration(color: Color(0xCC1A1A1A)),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
        child: Text(
          text,
          style: context.labelStyle.copyWith(color: const Color(0xFFFFFFFF)),
        ),
      ),
    );
  }
}

