/**
 * The fleet, as the fleet.
 *
 * Who a broadcast reaches is drawn as the screens themselves — every panel a
 * small bezel, the reached ones lit, the missed ones outlined in ochre — rather
 * than as a count with names under it. The count is still stated, because a
 * lower bound is a fact; but the shape of the reach is the shape of the room,
 * and the miss is where somebody's eye goes first in an emergency.
 *
 * The same object, inverted, is a screen's page asking which audiences include
 * it: each audience drawn as a footprint with this one screen lit.
 */

import { useFocus, litProps } from "./focus";
import type { SignageScreen } from "@/utils/lait/types";

export function Footprint({
  screens,
  reached,
  onOpen,
  size = "xs",
  className,
}: {
  screens: SignageScreen[];
  /** Ids the rule reaches. Everything else is the miss. */
  reached: Set<string>;
  onOpen?: (screen: SignageScreen) => void;
  size?: "xs" | "sm";
  className?: string;
}) {
  const { held } = useFocus();
  return (
    <div className={`ds-footprint is-${size}${className ? ` ${className}` : ""}`}>
      {screens.map((screen) => {
        const hit = reached.has(screen.id);
        return (
          <button
            type="button"
            key={screen.id}
            className="ds-footprint-cell"
            data-reached={hit || undefined}
            data-missed={!hit || undefined}
            {...litProps(held, held?.kind === "screen" && held.id === screen.id)}
            title={screen.name}
            aria-label={`${screen.name}: ${hit ? "reached" : "not reached"}`}
            onClick={onOpen ? () => onOpen(screen) : undefined}
            disabled={!onOpen}
          >
            <span className="ds-footprint-glass" />
          </button>
        );
      })}
    </div>
  );
}
