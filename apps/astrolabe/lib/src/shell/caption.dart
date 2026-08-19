/// The window's own controls: minimise, maximise/restore, close — and the bar
/// itself, which is what you drag.
///
/// **Windows only.** The window has no system title bar there, so nothing
/// draws these but this file. What that buys is one surface at the top of the
/// window instead of two in colours the theme never agreed on; what it costs
/// is the system menu on `Alt+Space`, and Windows 11's snap-layout flyout,
/// which needs a hit-test reply no Flutter window plugin offers.
///
/// macOS keeps its traffic lights under a hidden title bar and they are the
/// controls that window shows, so the frame draws none of this there — see
/// `systemDrawsWindowControls` in `window.dart`.
///
/// ## The marks are painted, not typed
///
/// Windows draws these glyphs from Segoe Fluent Icons at private-use code
/// points, and the Unicode near-misses (`─ ☐ ✕`) are a different weight from
/// each other in every font that carries all three. Four line segments and two
/// rectangles are fewer moving parts than a font fallback chain, and they stay
/// crisp at any scale factor.
///
/// ## Closing is not stopping
///
/// The close control raises an intent and never decides what closing means.
/// The shared window frame applies the policy: the primary client hides to the
/// tray so its Spaces keep converging, while disposable secondary windows exit.
library;

import 'package:covalence/covalence.dart';
import 'package:flutter/widgets.dart';

/// One control's width.
///
/// This was 46 — Windows' own caption metric — on the argument that a person
/// aims at the top-right corner with muscle memory built on every other window
/// on the machine. That argument holds for the *corner*, which the close
/// control still owns and still hits at any width: the corner is a point, and
/// Fitts' law makes it free however wide the button around it is.
///
/// What 46 actually bought was 138 points of a 32-point-tall bar spent on three
/// controls, in a client whose caption is a utility tier rather than a title
/// bar. The system metric is sized for a 32-point-tall *system* caption with a
/// title in it; ours carries a wordmark, a screen control and nothing else.
const double kCaptionWidth = 36;

/// The mark inside it — pinned like every other mark, because how big a close
/// cross reads is a claim about the cross rather than about the bar around it.
const double kCaptionMark = 9;

/// How wide the three of them are together.
const double kCaptionSpan = kCaptionWidth * 3;

enum _Mark { minimise, maximise, restore, close }

/// The three controls, flush with the window's corner.
///
/// Nothing may sit between these and the corner: a maximised window's corner is
/// the easiest pixel on the screen to hit, and a cluster inset from it by even
/// a point gives that up.
class CaptionControls extends StatelessWidget {
  const CaptionControls({
    super.key,
    required this.height,
    required this.maximised,
    required this.onMinimise,
    required this.onToggleMaximise,
    required this.onClose,
    this.closeTooltip = 'Close (it keeps serving in the tray)',
  });

  final double height;
  final bool maximised;
  final VoidCallback onMinimise;

  /// `null` means this window cannot be maximised, and the control is not
  /// drawn at all. A disabled maximise button would advertise a capability
  /// the HWND refuses; absence is the honest shape, and it is what a
  /// fixed-format window like the address book shows.
  final VoidCallback? onToggleMaximise;
  final VoidCallback onClose;
  final String closeTooltip;

  @override
  Widget build(BuildContext context) {
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        _CaptionButton(
          height: height,
          mark: _Mark.minimise,
          semanticLabel: 'Minimise',
          tooltip: 'Out of the way',
          onPressed: onMinimise,
        ),
        if (onToggleMaximise != null)
          _CaptionButton(
            height: height,
            mark: maximised ? _Mark.restore : _Mark.maximise,
            semanticLabel: maximised ? 'Restore' : 'Maximise',
            tooltip: maximised
                ? 'Restore the window to its previous size'
                : 'Fill the screen',
            onPressed: onToggleMaximise!,
          ),
        _CaptionButton(
          height: height,
          mark: _Mark.close,
          semanticLabel: 'Close',
          tooltip: closeTooltip,
          danger: true,
          onPressed: onClose,
        ),
      ],
    );
  }
}

class _CaptionButton extends StatefulWidget {
  const _CaptionButton({
    required this.height,
    required this.mark,
    required this.semanticLabel,
    required this.tooltip,
    required this.onPressed,
    this.danger = false,
  });

  final double height;
  final _Mark mark;
  final String semanticLabel;
  final String tooltip;
  final VoidCallback onPressed;
  final bool danger;

  @override
  State<_CaptionButton> createState() => _CaptionButtonState();
}

class _CaptionButtonState extends State<_CaptionButton> {
  bool _hovered = false;
  bool _pressed = false;

  @override
  Widget build(BuildContext context) {
    // The fill under a control whose click ends something is the theme's own
    // error colour, not a fixed `#C42B1C`: a hard-coded red is invisible in
    // exactly the scheme somebody chose because they could not see.
    final Color? fill = switch ((_pressed, _hovered, widget.danger)) {
      (true, _, true) => Err.l900.resolve(context),
      (_, true, true) => Err.l800.resolve(context),
      (true, _, false) => Surface.l200.resolve(context),
      (_, true, false) => Surface.l100.resolve(context),
      _ => null,
    };
    // The red is chromatic and does not invert with polarity, so the ink on it
    // is `onSolid` rather than a surface step — the same rule the design
    // system's own destructive button follows.
    final Color ink = widget.danger && (_hovered || _pressed)
        ? Surface.onSolid.resolve(context)
        : Glyph.l950.resolve(context);

    return Semantics(
      button: true,
      label: widget.semanticLabel,
      child: Tooltip(
        message: widget.tooltip,
        child: MouseRegion(
          cursor: SystemMouseCursors.basic,
          onEnter: (_) => setState(() => _hovered = true),
          onExit: (_) => setState(() {
            _hovered = false;
            _pressed = false;
          }),
          child: GestureDetector(
            onTapDown: (_) => setState(() => _pressed = true),
            onTapCancel: () => setState(() => _pressed = false),
            onTap: () {
              setState(() => _pressed = false);
              widget.onPressed();
            },
            child: CustomPaint(
              // Square, and to the edge. A rounded fill would leave the
              // window's own corner unpainted at exactly the pixel a maximised
              // window is aimed at.
              painter: _CaptionPainter(mark: widget.mark, fill: fill, ink: ink),
              child: context.tokens.box.sized(
                // reason: a caption button is sized to the Windows shell's own
                // metric rather than to this scale. It sits against the chrome
                // the OS draws, so it has to match that, not our rhythm.
                width: TokenEscape.rawSize(kCaptionWidth),
                // reason: the caption height is the title bar's, measured at
                // runtime by the window rather than chosen here.
                height: TokenEscape.rawSize(widget.height),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _CaptionPainter extends CustomPainter {
  const _CaptionPainter(
      {required this.mark, required this.fill, required this.ink});

  final _Mark mark;
  final Color? fill;
  final Color ink;

  @override
  void paint(Canvas canvas, Size size) {
    if (fill != null) {
      canvas.drawRect(Offset.zero & size, Paint()..color = fill!);
    }
    final stroke = Paint()
      ..color = ink
      ..strokeWidth = 1
      ..isAntiAlias = false
      ..style = PaintingStyle.stroke;

    // Rounded to whole pixels: these are one-pixel lines, and half a pixel out
    // turns a crisp hairline into two grey ones — at this size the difference
    // between a system glyph and a smudge.
    final centre = Offset(
      (size.width / 2).roundToDouble(),
      (size.height / 2).roundToDouble(),
    );
    final half = kCaptionMark / 2;
    final box = Rect.fromCenter(
        center: centre, width: kCaptionMark, height: kCaptionMark);

    switch (mark) {
      case _Mark.minimise:
        canvas.drawLine(
          Offset(centre.dx - half, centre.dy + 0.5),
          Offset(centre.dx + half, centre.dy + 0.5),
          stroke,
        );
      case _Mark.maximise:
        canvas.drawRect(box.deflate(0.5), stroke);
      case _Mark.restore:
        // Two sheets: the one in front, and the corner of the one behind it
        // showing past its top-right. Drawn as two segments rather than a
        // second rectangle so nothing has to be filled — a filled backing sheet
        // would have to know the hover colour underneath it.
        const offset = 3.0;
        final front = Rect.fromLTRB(
          box.left,
          box.top + offset,
          box.right - offset,
          box.bottom,
        );
        canvas.drawRect(front.deflate(0.5), stroke);
        canvas.drawLine(
          Offset(front.left + offset, box.top + 0.5),
          Offset(box.right - 0.5, box.top + 0.5),
          stroke,
        );
        canvas.drawLine(
          Offset(box.right - 0.5, box.top + 0.5),
          Offset(box.right - 0.5, front.bottom - offset),
          stroke,
        );
      case _Mark.close:
        canvas.drawLine(box.topLeft, box.bottomRight, stroke);
        canvas.drawLine(box.topRight, box.bottomLeft, stroke);
    }
  }

  @override
  bool shouldRepaint(_CaptionPainter old) =>
      old.mark != mark || old.fill != fill || old.ink != ink;
}
