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
 * it: each audience drawn as a footprint with this one screen lit. And in the
 * composer it is the control: pressing a panel names it.
 */

import { useFocus, litProps } from "./focus";
import type { SignageScreen } from "@/utils/lait/types";

export function Footprint({
  screens,
  reached,
  onOpen,
  size = "xs",
  named = false,
  className,
}: {
  screens: SignageScreen[];
  /** Ids the rule reaches. Everything else is the miss. Absent means no rule
   *  is being asked about: the fleet is drawn as it is, nothing lit, nothing
   *  missed. */
  reached?: Set<string>;
  onOpen?: (screen: SignageScreen) => void;
  size?: "xs" | "sm";
  /** Print each panel's name under it — for a footprint that is a control. */
  named?: boolean;
  className?: string;
}) {
  const { held } = useFocus();
  return (
    <div className={`ds-footprint is-${size}${className ? ` ${className}` : ""}`}>
      {screens.map((screen) => {
        const hit = reached ? reached.has(screen.id) : null;
        return (
          <button
            type="button"
            key={screen.id}
            className="ds-footprint-cell"
            data-reached={hit === true || undefined}
            data-missed={hit === false || undefined}
            {...litProps(held, held?.kind === "screen" && held.id === screen.id)}
            title={screen.name}
            aria-label={hit == null ? screen.name : `${screen.name}: ${hit ? "reached" : "not reached"}`}
            aria-pressed={onOpen && hit != null ? hit : undefined}
            onClick={onOpen ? () => onOpen(screen) : undefined}
            disabled={!onOpen}
          >
            <span className="ds-footprint-glass" />
            {named && <small>{screen.name}</small>}
          </button>
        );
      })}
    </div>
  );
}
