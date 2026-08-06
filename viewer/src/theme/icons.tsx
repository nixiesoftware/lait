/**
 * lait's icon set, handed to Astryx.
 *
 * WHY THIS EXISTS. Astryx components do not import icons — they resolve a
 * semantic NAME through the active theme's registry. `<Icon icon="funnel" />`
 * inside its own `Table` header is the same lookup our `FilterMenu` would do,
 * so whoever fills the registry decides what the whole app's chrome looks like.
 * Left empty, Astryx uses its own glyphs and the migrated half of the app grows
 * a second icon set beside the lucide one the other half already has.
 *
 * So we fill it. All 28 names, one lucide element each.
 *
 * WHY ELEMENTS AND NOT COMPONENTS. `IconRegistry` is `Record<IconName,
 * ReactNode>`, so these are rendered nodes. Size and colour are deliberately
 * absent: Astryx's `<Icon>` sizes its child through CSS (`data-size`), and our
 * glyph axis is PINNED to the type scale rather than the spacing unit — see
 * the tripwire in `designSystem.test.ts`. Baking a pixel size in here would put
 * a number on the wrong axis and the density toggle would start fattening
 * glyphs again, which is the exact bug that guard exists to catch.
 *
 * WHAT IS NOT HERE. `ui/icons.tsx` — the priority bars and the status ring.
 * Those are data encoded as shape rather than decoration, they have no lucide
 * equivalent, and they are not in Astryx's vocabulary either. They stay ours.
 */

import type { IconRegistry } from "@astryxdesign/core/Icon";
import {
  ArrowDown,
  ArrowUp,
  ArrowUpDown,
  Calendar,
  Check,
  CheckCheck,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronsLeft,
  ChevronsRight,
  CircleAlert,
  CircleCheck,
  CircleStop,
  Clock,
  Columns3,
  Copy,
  ExternalLink,
  EyeOff,
  Funnel,
  Info,
  Menu,
  Mic,
  MoreHorizontal,
  Search,
  TriangleAlert,
  Wrench,
  X,
} from "lucide-react";

export const laitIcons: IconRegistry = {
  close: <X aria-hidden />,
  chevronDown: <ChevronDown aria-hidden />,
  chevronLeft: <ChevronLeft aria-hidden />,
  chevronRight: <ChevronRight aria-hidden />,
  chevronsLeft: <ChevronsLeft aria-hidden />,
  chevronsRight: <ChevronsRight aria-hidden />,
  check: <Check aria-hidden />,

  // Status. `success`/`error`/`warning` are the filled-meaning trio Astryx
  // uses in Banner and StatusDot; ours read as outline glyphs everywhere else,
  // so they stay outline here rather than becoming three solid discs the rest
  // of the app has no precedent for.
  success: <CircleCheck aria-hidden />,
  error: <CircleAlert aria-hidden />,
  warning: <TriangleAlert aria-hidden />,
  info: <Info aria-hidden />,

  calendar: <Calendar aria-hidden />,
  clock: <Clock aria-hidden />,
  externalLink: <ExternalLink aria-hidden />,
  menu: <Menu aria-hidden />,
  moreHorizontal: <MoreHorizontal aria-hidden />,
  search: <Search aria-hidden />,

  // Table chrome. `arrowsUpDown` is the resting state of a sortable column and
  // the one most likely to be seen a hundred times a session.
  arrowUp: <ArrowUp aria-hidden />,
  arrowDown: <ArrowDown aria-hidden />,
  arrowsUpDown: <ArrowUpDown aria-hidden />,
  funnel: <Funnel aria-hidden />,
  eyeSlash: <EyeOff aria-hidden />,
  viewColumns: <Columns3 aria-hidden />,

  copy: <Copy aria-hidden />,
  checkDouble: <CheckCheck aria-hidden />,
  wrench: <Wrench aria-hidden />,
  stop: <CircleStop aria-hidden />,
  microphone: <Mic aria-hidden />,
};
