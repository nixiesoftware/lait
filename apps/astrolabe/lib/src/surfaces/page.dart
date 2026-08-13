/// The shape every surface has: a name, a line saying what it is for, an
/// optional control that belongs to the page rather than to a row, and the page
/// itself.
///
/// One scaffold rather than seven near-identical headers, because the rhythm
/// between a title and its prose and its content is part of the visual language
/// and not a decision each page gets to make differently.
library;

import 'package:flutter/widgets.dart';

import '../shell/type.dart';

class SurfaceScaffold extends StatelessWidget {
  const SurfaceScaffold({
    super.key,
    required this.title,
    required this.prose,
    required this.child,
    this.trailing,
  });

  final String title;

  /// One line. The pages this replaced explained themselves in three, in grey,
  /// at the label size — which is how a surface ends up with the least readable
  /// thing on it being the thing that explains it.
  final String prose;

  final Widget child;

  /// The one control that acts on the whole page, when there is one.
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(title, style: context.titleStyle),
                  const SizedBox(height: 4),
                  Text(prose, style: context.proseStyle),
                ],
              ),
            ),
            if (trailing != null) trailing!,
          ],
        ),
        const SizedBox(height: 20),
        Expanded(child: child),
      ],
    );
  }
}

/// A read that succeeded and answered nothing.
///
/// Deliberately not the same widget as a loading state, and deliberately
/// carrying a second line: "there is nothing here" is only useful next to "and
/// here is how something gets here".
class Empty extends StatelessWidget {
  const Empty({super.key, required this.said, this.next});

  final String said;
  final String? next;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(said, style: context.bodyStyle),
        if (next != null) ...[
          const SizedBox(height: 4),
          Text(next!, style: context.proseStyle),
        ],
      ],
    );
  }
}
