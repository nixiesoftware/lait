import type React from "react";

import { cn } from "./cn";

/**
 * A keyboard chord, already formatted.
 *
 * Named `Chord`, not `Kbd`, for two reasons. It would collide with core's `Kbd`
 * — the CLI cannot answer `astryx component Kbd` when two packages provide one
 * — and more to the point it is not the same component.
 *
 * Astryx's `Kbd` takes a `keys` string in its own grammar (`mod+k`, `escape`)
 * and formats it platform-aware. This renders a chord that lait's key registry
 * has already formatted
 * — the registry is the source of truth for what a shortcut IS, so it has to be
 * the source of truth for how the shortcut READS. Two formatters over one
 * binding is how a menu ends up disagreeing with the palette.
 */
export function Chord({
  children,
  className,
}: {
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <kbd
      className={cn(
        "border-line-strong bg-bg text-dim rounded-mark border px-1 font-mono text-2xs leading-4",
        className,
      )}
    >
      {children}
    </kbd>
  );
}
