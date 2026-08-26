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
 * without the section having to be opened to find out — which is the whole
 * point of the dot, and the reason this is not "is a link set".
 */
export function isModified(world: LibraryWorld | null): boolean {
  return world !== null && (world.linked !== null || world.channel !== null);
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

function GeneralPane({ snapshot }: { snapshot: WorldSettingsSnapshot }) {
  return <>
    <h1>{snapshot.name}</h1>
    <p className="settings-prose">Runtime and location details reported by this World.</p>
    <SettingsSection title="APPLICATION">
      <Setting label="IMPLEMENTATION VERSION"
        value={snapshot.version === null ? "Not reported" : `v${snapshot.version}`} />
    </SettingsSection>
    <SettingsSection title="LOCATIONS">
      <Setting label="WORLD MOUNT" value={snapshot.worldMount} mono />
      <Setting label="ENTRY PATH" value={snapshot.entryPath ?? "Not declared"} mono />
    </SettingsSection>
  </>;
}

function InstancePane({ snapshot }: { snapshot: WorldSettingsSnapshot }) {
  return <>
    <h1>Instance</h1>
    <p className="settings-prose">Where this World is being served from right now.</p>
    <SettingsSection title="ACTIVE INSTANCE">
      <Setting label="ORIGIN" value={snapshot.activeOrigin ?? "Not reported"} mono />
    </SettingsSection>
  </>;
}

/**
 * Developer options.
 *
 * Always present, never hidden behind a gesture: a mode that has to be
 * discovered is a mode nobody audits. What is deliberate is *entering* it —
 * one switch, remembered per World — and what it turns on is stated before it
 * is offered.
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
    // the two are different facts. Offering controls that cannot dispatch
    // would be the worse of the two.
    return <>
      <h1>Developer</h1>
      <p className="settings-prose settings-absent">
        This window could not reach Astrolabe, so these options cannot be offered.
        The details on the other pages are from when the window opened.
      </p>
    </>;
  }

  return <>
    <h1>Developer</h1>
    <p className="settings-prose">
      Options for working on {snapshot.name} itself. They change what this device
      serves and follows — nobody else's copy of this World is affected.
    </p>
    <label className="settings-switch-row">
      <span className="settings-switch-label">Developer options</span>
      <input type="checkbox" checked={enabled} onChange={(event) => enable(event.target.checked)} />
    </label>
    {enabled && world !== null && <>
      <SourceCard snapshot={snapshot} world={world} view={view} dispatch={dispatch} />
      <ChannelCard world={world} view={view} dispatch={dispatch} />
    </>}
    {enabled && world === null && <p className="settings-prose settings-absent">
      This World is not in the Library this client is showing, so there is nothing
      to point at yet.
    </p>}
  </>;
}

/**
 * Where this World's page is read from.
 *
 * The banner is the point of the card. A machine serving somebody's working
 * tree while believing it serves a release is the defect the whole seam exists
 * not to produce, and a log line the person never reads is not how it gets
 * avoided.
 */
function SourceCard({ snapshot, world, view, dispatch }: {
  snapshot: WorldSettingsSnapshot;
  world: LibraryWorld;
  view: ClientView;
  dispatch(action: ClientAction): Promise<void>;
}) {
  const [draft, setDraft] = useState(world.linked ?? "");
  const busy = view.inFlight.includes(actionKey.linkWorld(world.worldMount));
  // Running, from the heads that name this World. Whether the running head
  // started before or after the record was written is not something this
  // window can know, so it does not claim to: it says what a restart is for
  // and leaves the fact alone.
  const running = view.heads.some((head) => head.world === world.worldMount && head.state === "running");

  return <SettingsSection title="SOURCE">
    {world.linked !== null && <p className="settings-warning">
      This World is served from a directory on this device, not from its
      release{snapshot.version === null ? "" : ` (v${snapshot.version})`}.
    </p>}
    <p className="settings-prose">
      A World is normally read from the signed release installed for it. Point it
      at a directory to work on its pages without publishing one.
    </p>
    <input
      className="settings-input"
      type="text"
      spellCheck={false}
      value={draft}
      placeholder="/path/to/the/built/pages"
      aria-label="Directory to serve this World from"
      onChange={(event) => setDraft(event.target.value)} />
    <div className="settings-actions">
      <button
        type="button"
        disabled={busy || draft.trim() === ""}
        onClick={() => void dispatch({ type: "linkWorld", world: world.worldMount, dir: draft.trim() })}>
        Serve from this directory
      </button>
      <button
        type="button"
        disabled={busy || world.linked === null}
        onClick={() => { setDraft(""); void dispatch({ type: "linkWorld", world: world.worldMount, dir: null }); }}>
        Serve the release
      </button>
    </div>
    <p className="settings-note">
      {running
        ? "This World is running. Stop it in the Library and open it again to serve what is set here."
        : "Takes effect the next time this World is opened."}
    </p>
  </SettingsSection>;
}

/**
 * Which stream this World follows.
 *
 * Per World, because a World is published on its own channel pointer. Its own
 * choice and the device's are different facts and the control says which one is
 * answering — "Test" because this World was set to test is not the same state
 * as "Test" because the device is on test, and a radio that showed them
 * identically would make the second look like a decision somebody made here.
 */
function ChannelCard({ world, view, dispatch }: {
  world: LibraryWorld;
  view: ClientView;
  dispatch(action: ClientAction): Promise<void>;
}) {
  const busy = view.inFlight.includes(actionKey.followWorldChannel(world.worldMount));
  const selection = channelSelection(world);
  const choose = (channel: string | null) =>
    void dispatch({ type: "followWorldChannel", world: world.worldMount, channel });

  return <SettingsSection title="CHANNEL">
    <p className="settings-prose">
      Which of this World's published streams this device follows when it checks
      for an update.
    </p>
    <ChannelChoice label="Follow this device" hint="Whatever the device is set to"
      checked={selection === "device"} busy={busy} onPick={() => choose(null)} />
    <ChannelChoice label="Stable" hint="Published releases"
      checked={selection === "stable"} busy={busy} onPick={() => choose("stable")} />
    <ChannelChoice label="Test" hint="Candidates, before they are promoted"
      checked={selection === "test"} busy={busy} onPick={() => choose("test")} />
    {selection === "unknown" && <p className="settings-note">
      This World is set to follow &ldquo;{world.channel}&rdquo;, which this build
      does not know. Choosing above replaces it.
    </p>}
  </SettingsSection>;
}

function ChannelChoice({ label, hint, checked, busy, onPick }: {
  label: string; hint: string; checked: boolean; busy: boolean; onPick(): void;
}) {
  return <label className="settings-choice">
    <input type="radio" checked={checked} disabled={busy} onChange={onPick} />
    <span className="settings-choice-label">{label}</span>
    <span className="settings-choice-hint">{hint}</span>
  </label>;
}

function SettingsSection({ title, children }: { title: string; children: React.ReactNode }) {
  return <section className="settings-card">
    <span className="fact-label">{title}</span>
    {children}
  </section>;
}

function Setting({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return <div className="setting-row">
    <span className="fact-label">{label}</span>
    {mono ? <code>{value}</code> : <span>{value}</span>}
  </div>;
}
