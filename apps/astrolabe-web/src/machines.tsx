/**
 * The Devices window: every machine attached to this identity or this
 * computer, in one place. A rail names the two kinds — your own devices, and
 * the TVs linked to this computer — and each pane keeps its own rules; the
 * window only navigates. Which section is showing is this window's one piece
 * of local state, and it starts on your devices because those are yours
 * everywhere, while TVs are a fact about this computer.
 */
import { useState } from "react";

import type { ClientAction, ClientView } from "./client";
import { DevicesPane } from "./devices";
import { DisplaysPane } from "./displays";
import { IconLaptop, IconTv } from "./icons";

type Dispatch = (action: ClientAction) => Promise<void>;

type MachinesSection = "devices" | "tvs";

export function MachinesSurface({ view, dispatch }: { view: ClientView; dispatch: Dispatch }) {
  const [section, setSection] = useState<MachinesSection>("devices");
  return <section className="rail-split" aria-label="Devices">
    <nav className="pane-rail" aria-label="Device kinds">
      <button type="button" className={section === "devices" ? "rail-item current" : "rail-item"}
        onClick={() => setSection("devices")}>
        <IconLaptop size={16} /> Your devices
      </button>
      <button type="button" className={section === "tvs" ? "rail-item current" : "rail-item"}
        onClick={() => setSection("tvs")}>
        <IconTv size={16} /> Linked TVs
      </button>
    </nav>
    {section === "devices"
      ? <DevicesPane view={view} dispatch={dispatch} />
      : <DisplaysPane view={view} dispatch={dispatch} />}
  </section>;
}
