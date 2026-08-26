/**
 * The World-settings window — deliberately separate from the client.
 *
 * Two kinds of thing live here and they are not mixed.
 *
 * **Facts** arrive as a read-only snapshot in the window's URL, complete, when
 * the window is summoned. They state what was true when it opened and they do
 * not move, which is what lets a settings window behave like desktop settings:
 * independently movable, focusable and closed.
 *
 * **Choices** cannot work that way, because a choice has to be carried
 * somewhere and its answer has to come back. So the pane that offers them —
 * and only that pane — attaches the same transport the client window does.
 *
 * That is not a second model. The rule this project holds is that the App owns
 * the state and every interface receives whole immutable `ClientView`
 * projections while holding nothing but drafts. A second window receiving the
 * same projection is a second *view*; the path being typed into a field is a
 * draft, which is exactly where a draft is supposed to live.
 */
import { useState } from "react";

import { actionKey, type ClientAction, type ClientView, type LibraryWorld, type WorldSettingsSnapshot } from "./client";

type Pane = "general" | "instance" | "developer";

/**
 * Whether anything under Developer is not what a fresh install would be.
 *
 * Drives the dot on the rail, so the window says it is in a modified state
 * without the section having to be opened to find out.
 */
export function isModified(world: LibraryWorld | null): boolean {
  return world !== null && world.channel !== null;
}

/**
 * Which channel row is answering.
 *
 * `"device"` is not the same state as the channel the device happens to be on.
 * A World following the device is following whatever that becomes; a World set
 * to test is set to test. Collapsing them would draw the first as a decision
 * somebody made here, which is the same class of mistake as reporting an
 * unmeasured thing as zero.
 */
export function channelSelection(world: LibraryWorld): "device" | "stable" | "test" | "unknown" {
  if (world.channel === null) return "device";
  if (world.channel === "stable") return "stable";
  if (world.channel === "test") return "test";
  // A channel this build does not know. Not silently "device": the World is
  // following something, and saying otherwise would be a confident wrong answer.
  return "unknown";
}

/** Remembered per World, so opening the window again lands where you left. */
function enabledKey(mount: string) {
  return `astrolabe.developer.${mount}`;
}

function readEnabled(mount: string): boolean {
  try {
    return localStorage.getItem(enabledKey(mount)) === "1";
  } catch {
    // A private window, or site data blocked. Developer options being off is
    // the correct answer when we cannot find out, and nothing here depends on
    // remembering it.
    return false;
  }
}

export function WorldSettingsSurface({
  snapshot,
  view,
  dispatch,
}: {
  snapshot: WorldSettingsSnapshot;
  /** Absent when the window could not attach — the facts still draw. */
  view: ClientView | null;
  dispatch(action: ClientAction): Promise<void>;
}) {
  const [pane, setPane] = useState<Pane>("general");
  const world = view?.library?.find((row) => row.worldMount === snapshot.worldMount) ?? null;
  // The dot on the rail. On whenever anything under Developer is not what a
  // fresh install would be, so the window says it is in a modified state
  // without the section having to be opened to find out.
  const modified = isModified(world);

  return <section className="settings-page settings-split" aria-label={`${snapshot.name} settings`}>
    <nav className="settings-rail" aria-label="Settings sections">
      <RailItem pane="general" current={pane} onPick={setPane} label="General" />
      <RailItem pane="instance" current={pane} onPick={setPane} label="Instance" />
      <RailItem pane="developer" current={pane} onPick={setPane} label="Developer" dot={modified} />
    </nav>
    <div className="settings-pane">
      {pane === "general" && <GeneralPane snapshot={snapshot} />}
      {pane === "instance" && <InstancePane snapshot={snapshot} />}
      {pane === "developer" && <DeveloperPane
        snapshot={snapshot}
        world={world}
        view={view}
        dispatch={dispatch} />}
    </div>
  </section>;
}

function RailItem({ pane, current, onPick, label, dot = false }: {
  pane: Pane; current: Pane; onPick(next: Pane): void; label: string; dot?: boolean;
}) {
  return <button
    type="button"
    className={`settings-rail-item${pane === current ? " current" : ""}`}
    aria-current={pane === current ? "page" : undefined}
    onClick={() => onPick(pane)}>
    <span>{label}</span>
    {dot && <span className="settings-rail-dot" aria-label="modified" />}
  </button>;
}

/**
 * A settings window, not a page.
 *
 * The rail names the section, so a pane does not repeat it in a heading, and
 * a row states its fact on one line instead of introducing it in a sentence
 * first. Prose here is a cost: at 560 wide it pushes the thing somebody came
 * for below the fold, and a person opening a settings window already knows
 * what settings are.
 *
 * What survives is the text that carries a fact nothing else can — an absence
 * that has to say which kind it is, and nothing else.
 */
function GeneralPane({ snapshot }: { snapshot: WorldSettingsSnapshot }) {
  return <Group>
    <Row label="Version" value={snapshot.version === null ? "Not reported" : `v${snapshot.version}`} />
    <Row label="Mount" value={snapshot.worldMount} mono />
    <Row label="Entry path" value={snapshot.entryPath ?? "Not declared"} mono />
  </Group>;
}

function InstancePane({ snapshot }: { snapshot: WorldSettingsSnapshot }) {
  return <Group>
    <Row label="Origin" value={snapshot.activeOrigin ?? "Not reported"} mono />
  </Group>;
}

/**
 * Developer options.
 *
 * Always present, never hidden behind a gesture: a mode that has to be
 * discovered is a mode nobody audits. Entering it is the deliberate part —
 * one switch, remembered per World.
 */
function DeveloperPane({ snapshot, world, view, dispatch }: {
  snapshot: WorldSettingsSnapshot;
  world: LibraryWorld | null;
  view: ClientView | null;
  dispatch(action: ClientAction): Promise<void>;
}) {
  const [enabled, setEnabled] = useState(() => readEnabled(snapshot.worldMount));
  const enable = (next: boolean) => {
    setEnabled(next);
    try {
      localStorage.setItem(enabledKey(snapshot.worldMount), next ? "1" : "0");
    } catch {
      // Convenience only; the switch still works for this window's life.
    }
  };

  if (view === null) {
    // Not "no developer options" — this window could not reach the client, and
    // the two are different facts.
    return <p className="settings-absent">Astrolabe is not reachable from this window.</p>;
  }

  return <>
    <Group>
      <SwitchRow label="Developer options" checked={enabled} onChange={enable} />
    </Group>
    {enabled && world !== null && <ChannelGroup world={world} view={view} dispatch={dispatch} />}
    {enabled && world === null && <p className="settings-absent">
      This World is not in the Library this client is showing.
    </p>}
  </>;
}

/**
 * Which stream this World follows.
 *
 * One row, one control. "Follow this device" is a separate option from the
 * channel the device happens to be on: one follows whatever the device
 * becomes, the other is a decision, and a control that drew them the same way
 * would report a default as a choice.
 */
function ChannelGroup({ world, view, dispatch }: {
  world: LibraryWorld;
  view: ClientView;
  dispatch(action: ClientAction): Promise<void>;
}) {
  const busy = view.inFlight.includes(actionKey.followWorldChannel(world.worldMount));
  const selection = channelSelection(world);
  return <Group>
    <div className="settings-row">
      <span className="settings-row-label">Channel</span>
      <select
        className="settings-select"
        aria-label="Channel"
        disabled={busy}
        value={selection}
        onChange={(event) => void dispatch({
          type: "followWorldChannel",
          world: world.worldMount,
          channel: event.target.value === "device" ? null : event.target.value,
        })}>
        <option value="device">Follow this device</option>
        <option value="stable">Stable</option>
        <option value="test">Test</option>
        {selection === "unknown" && <option value="unknown" disabled>{world.channel}</option>}
      </select>
    </div>
  </Group>;
}

/** A grouped card: the desktop idiom, rows divided by a hairline. */
function Group({ children }: { children: React.ReactNode }) {
  return <section className="settings-group">{children}</section>;
}

/** Label left, fact right, one line. */
function Row({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return <div className="settings-row">
    <span className="settings-row-label">{label}</span>
    {mono ? <code className="settings-row-value">{value}</code>
      : <span className="settings-row-value">{value}</span>}
  </div>;
}

function SwitchRow({ label, checked, onChange }: {
  label: string; checked: boolean; onChange(next: boolean): void;
}) {
  return <label className="settings-row">
    <span className="settings-row-label">{label}</span>
    <input type="checkbox" role="switch" checked={checked}
      onChange={(event) => onChange(event.target.checked)} />
  </label>;
}
