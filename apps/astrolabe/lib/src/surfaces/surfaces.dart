/// The page furniture surfaces share.
///
/// The client is the Library now — one surface, no navigation between
/// destinations that no longer exist. Spaces, Members, and the Operations
/// group were removed whole: the client surfaces lifecycle, the address book
/// carries people, and each World carries its own settings. What remains
/// here is the margin every conventional page keeps.
library;

import 'package:covalence/covalence.dart';
import 'package:flutter/widgets.dart';

/// The page margin. A surface flush against the window edge reads as
/// unfinished whatever else is right about it.
///
/// A function of the theme rather than a constant: the margin is a spatial rung
/// like every other measurement here, and a baked `16` would be the one that
/// stopped answering when the scale was retuned.
EdgeInsets pageMargin(Tokens t) =>
    t.padding.fromLTRB(Space.xl3, Space.xl, Space.xl3, Space.xl3);
