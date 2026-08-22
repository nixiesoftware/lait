import { useCallback, useEffect, useMemo, useState } from "react";
import { Button, Menu, MenuItem, MenuTrigger, Popover, Separator } from "react-aria-components";

import {
  actionKey,
  closeOwnedWindow,
  createClientTransport,
  currentOwnedWindowSurface,
  currentWorldSettingsSnapshot,
  keyFor,
  loadingClientView,
  setFullscreen,
  summonOwnedWindow,
  restartForUpdate,
  summonWorldSettings,
  updateInProgress,
  watchMenu,
  worldArtwork,
  type ClientAction,
  type ClientTransport,
  type ClientView,
  type Head,
  type LibraryWorld,
  type OwnedWindowSurface,
  type UpdateIntent,
  type WorldArtwork,
  type WorldPerson,
} from "./client";
import { BookSurface } from "./book";
import { ChatSurface } from "./chat";
import { DisplaysSurface } from "./displays";
import { BigPictureSurface } from "./present";
import { WorldSettingsSurface } from "./settings";
import { FacePlate, PersonTile, presenceLabel } from "./kit";
import { activityLine, glanceTiers, identityStatus } from "./record";
import { resolvePlatform, type PlatformProfile } from "./platform";

const utilityBarHeight = 32;
const railWidth = 224;
const heroHeight = 196;

export function App() {
  const [platform] = useState<PlatformProfile>(() => resolvePlatform());
  const [dark, setDark] = useState(true);
  // The settings window never attaches a transport: its snapshot arrived in
  // the URL, complete, when the window was summoned.
  const settingsSnapshot = useMemo(() => currentWorldSettingsSnapshot(), []);
  if (settingsSnapshot !== null) {
    return <main className="page owned-window" data-theme={settingsSnapshot.dark ? "dark" : "light"}>
      <WorldSettingsSurface snapshot={settingsSnapshot} />
    </main>;
  }
  return <ClientApp platform={platform} dark={dark} setDark={setDark} />;
}

function ClientApp({ platform, dark, setDark }: { platform: PlatformProfile; dark: boolean; setDark(next: boolean): void }) {
  const transport = useMemo(() => createClientTransport(), []);
  const { view, dispatch } = useClient(transport);
  const [selected, setSelected] = useState<string | null>(null);
  const ownedSurface = currentOwnedWindowSurface();
  const refreshing = view.inFlight.includes(actionKey.refresh);

  useEffect(() => { document.documentElement.dataset.platform = platform; }, [platform]);
  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      // The same refusal the menu applies: a re-read already in flight is a
      // key that does nothing, not one that queues a second read.
      if (event.key === "F5") {
        event.preventDefault();
        if (!refreshing) void dispatch({ type: "refresh" });
      }
      if ((event.metaKey || event.ctrlKey) && event.shiftKey) {
        const target = event.key.toLowerCase() === "b" ? "book"
          : event.key.toLowerCase() === "d" ? "displays"
          : event.key.toLowerCase() === "m" ? "chat"
          : null;
        if (target !== null) { event.preventDefault(); void summonOwnedWindow(target); }
      }
    };
    window.addEventListener("keydown", shortcut);
    return () => window.removeEventListener("keydown", shortcut);
  }, [dispatch, refreshing]);

  // The native application menu's model-facing items land here: the menu is
  // the OS's, but the model is this page's.
  useEffect(() => watchMenu((id) => {
    if (id === "refresh" && !refreshing) void dispatch({ type: "refresh" });
    if (id === "theme") setDark(!dark);
  }), [dispatch, refreshing, dark, setDark]);

  const worlds = view.library;
  const showing = useMemo(
    () => worlds?.find((world) => world.key === selected) ?? worlds?.[0] ?? null,
    [selected, worlds],
  );

  if (ownedSurface !== null) return <OwnedSurfaceWindow surface={ownedSurface} view={view} dispatch={dispatch} dark={dark} />;

  // Big Picture replaces the window rather than filling it. The client's own
  // chrome is exactly what a screen must not have, so the caption and the
  // operational bar are absent here — not hidden behind it.
  if (view.presentation !== null) {
    return <main className="page presenting">
      <BigPictureSurface presentation={view.presentation} view={view} dispatch={dispatch} />
    </main>;
  }

  return <main className="page" data-theme={dark ? "dark" : "light"}>
    <section className="astrolabe-window" aria-label="Astrolabe">
      <Caption platform={platform} dark={dark} setDark={setDark} onSummonWindow={summonOwnedWindow}
        version={view.host?.version ?? null} loading={view.loading}
        onPresent={() => void dispatch({ type: "enterPresentation" })}
        refreshing={refreshing} onRefresh={() => void dispatch({ type: "refresh" })} />
      <div className="client-body">
        <Library view={view} showing={showing} onSelect={setSelected} dispatch={dispatch} dark={dark} />
        <OperationalBar view={view} onSummonWindow={summonOwnedWindow} dispatch={dispatch} />
      </div>
    </section>
  </main>;
}

/** Attaches the primary surface to whole snapshots from the desktop bridge. */
function useClient(transport: ClientTransport) {
  const [view, setView] = useState<ClientView>(loadingClientView);

  useEffect(() => {
    let alive = true;
    const stop = transport.watch((next) => { if (alive) setView(next); });
    void transport.current().then(
      (next) => { if (alive) setView(next); },
      (error: unknown) => {
        if (!alive) return;
        setView((current) => withDispatchFailure(current, "Read local state", error));
      },
    );
    return () => { alive = false; stop(); };
  }, [transport]);

  const dispatch = useCallback(async (action: ClientAction) => {
    const key = keyFor(action);
    // The core returns this snapshot itself. This short optimistic overlay only
    // covers the IPC turn, preserving Flutter's disabled-on-click invariant.
    setView((current) => current.inFlight.includes(key)
      ? current
      : { ...current, inFlight: [...current.inFlight, key] });
    try {
      setView(await transport.dispatch(action));
    } catch (error) {
      setView((current) => withDispatchFailure(current, `Dispatch ${action.type}`, error, key));
    }
  }, [transport]);

  return { view, dispatch };
}

function withDispatchFailure(view: ClientView, what: string, error: unknown, actionKeyToClear?: string): ClientView {
  return {
    ...view,
    inFlight: actionKeyToClear === undefined ? view.inFlight : view.inFlight.filter((key) => key !== actionKeyToClear),
    failures: [{ what, error: error instanceof Error ? error.message : String(error), retryable: true }, ...view.failures],
  };
}

function OwnedSurfaceWindow({ surface, view, dispatch, dark }: { surface: OwnedWindowSurface; view: ClientView; dispatch(action: ClientAction): Promise<void>; dark: boolean }) {
  const refreshing = view.inFlight.includes(actionKey.refresh);
  // The owned windows carry no menu, and Book and Chat draw no refresh
  // control — F5 is the one way to ask for a re-read, with the same refusal
  // as the main window's: a re-read already in flight is a key that does
  // nothing, not one that queues a second read.
  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      if (event.key === "F5") {
        event.preventDefault();
        if (!refreshing) void dispatch({ type: "refresh" });
      }
    };
    window.addEventListener("keydown", shortcut);
    return () => window.removeEventListener("keydown", shortcut);
  }, [dispatch, refreshing]);
  const close = () => { void closeOwnedWindow(); };
  return <main className="page owned-window" data-theme={dark ? "dark" : "light"}>
    {surface === "book" && <BookSurface view={view} dispatch={dispatch} onBack={close} ownedWindow />}
    {surface === "displays" && <DisplaysSurface view={view} dispatch={dispatch} onBack={close} ownedWindow />}
    {surface === "chat" && <ChatSurface view={view} dispatch={dispatch} onBack={close} ownedWindow />}
  </main>;
}

/**
 * The utility tier. Window controls are the OS's own — the native header on
 * Windows, the traffic lights on macOS — and this strip draws none: it holds
 * the application menu where the system does not carry one, and the rest of
 * it is a drag surface.
 */
function Caption({ platform, dark, setDark, refreshing, version, loading, onRefresh, onPresent, onSummonWindow }: {
  platform: PlatformProfile; dark: boolean; setDark(next: boolean): void; refreshing: boolean;
  version: string | null; loading: boolean; onRefresh(): void; onPresent(): void;
  onSummonWindow(surface: OwnedWindowSurface): void;
}) {
  const systemMenu = platform === "macos";
  return <header className="caption" data-tauri-drag-region style={{ height: utilityBarHeight }}>
    {systemMenu ? <div className="traffic-light-clearance" /> : <MenuTrigger>
      <Button className="wordmark" aria-label="Astrolabe settings">ASTROLABE</Button>
      <Popover className="settings-popover"><Menu className="settings-menu" aria-label="Astrolabe settings">
        <header className="settings-header"><strong>ASTROLABE</strong>{version !== null && <code>v{version}</code>}</header><Separator />
        <span className="settings-section">CLIENT SETTINGS</span>
        <MenuItem id="displays" onAction={() => void onSummonWindow("displays")}>Displays <kbd>⌘⇧D</kbd></MenuItem>
        <MenuItem id="refresh" isDisabled={refreshing} onAction={onRefresh}>Refresh local state <kbd>F5</kbd></MenuItem>
        <MenuItem id="theme" onAction={() => setDark(!dark)}>{dark ? "Use light theme" : "Use dark theme"}</MenuItem>
      </Menu></Popover>
    </MenuTrigger>}
    <div className="caption-drag" />
    <PresentHere loading={loading} onPresent={onPresent} />
  </header>;
}

/**
 * Enter Big Picture. Pressing it enters, full stop — no dialog stands in
 * front of it asking what to show, because that is the wrong order: a person
 * presses this to *become* a screen, and choosing is what they do once they
 * are one. The press is the consent.
 */
function PresentHere({ loading, onPresent }: { loading: boolean; onPresent(): void }) {
  // A native button, deliberately: the press is the consent, and in a browser
  // it is also the one moment fullscreen may be taken — so it is taken here,
  // inside the native click's own activation, rather than after the
  // round-trip that answers the mode or behind a synthetic press event that
  // a browser may not honour as a gesture.
  return <span className="tip present-tip" title={loading ? "Still reading this machine." : "Make this machine a screen."}>
    <button className="present-control" aria-label="Present on this screen" disabled={loading} onClick={() => {
      void setFullscreen(true);
      onPresent();
    }}>
      <ScreenMark />
    </button>
  </span>;
}

/**
 * A screen, painted rather than typed. Not the four maximise arrows — that is
 * the OS header's idea, one cluster over. A monitor says the other thing: not
 * *bigger*, but *a screen* — this machine showing a World rather than
 * launching one.
 */
function ScreenMark() {
  return <svg width="14" height="12" viewBox="0 0 14 12" aria-hidden fill="none"
    stroke="currentColor" strokeWidth="1.25">
    <rect x="0.5" y="0.5" width="13" height="7.5" rx="1.5" />
    <line x1="7" y1="8" x2="7" y2="10.5" />
    <line x1="4" y1="11" x2="10" y2="11" />
  </svg>;
}

function Library({ view, showing, onSelect, dispatch, dark }: {
  view: ClientView;
  showing: LibraryWorld | null;
  onSelect(key: string): void;
  dispatch(action: ClientAction): Promise<void>;
  dark: boolean;
}) {
  if (view.library === null) return <LoadingLibrary />;
  if (view.library.length === 0) return <EmptyLibrary />;
  // A launching World sits with the running: the act is already under way,
  // and the rail should not file it under the ordinary rows mid-transition.
  // Per World, by the head's own state — a global "any head exists" check is
  // how opening Issues once filed Signage under RUNNING.
  const running = view.library.filter((world) => {
    const state = lifecycle(view, world);
    return state === "Launching" || state === "Running";
  });
  const ready = view.library.filter((world) => !running.includes(world) && world.opensAt !== null);
  const unavailable = view.library.filter((world) => !running.includes(world) && world.opensAt === null);
  return <section className="library">
    <aside className="library-rail" style={{ width: railWidth }}>
      <div className="library-heading"><span>LIBRARY</span><span>{view.library.length}</span></div>
      <div className="world-sections">
        {running.length > 0 && <WorldSection label="RUNNING" rows={running} view={view} showing={showing} onSelect={onSelect} />}
        {ready.length > 0 && <WorldSection rows={ready} view={view} showing={showing} onSelect={onSelect} />}
        {unavailable.length > 0 && <WorldSection label="UNAVAILABLE" rows={unavailable} view={view} showing={showing} onSelect={onSelect} />}
      </div>
    </aside>
    {showing !== null && <WorldDetail view={view} world={showing} dispatch={dispatch} dark={dark} />}
  </section>;
}

/**
 * The artwork a World compiled in, read once per mount. The initial render
 * draws the accent fallback and the art arrives a frame later — the same
 * fallback a World that ships no art keeps for good.
 */
function useWorldArtwork(mount: string): WorldArtwork {
  const [art, setArt] = useState<WorldArtwork>({ mark: null, hero: null });
  useEffect(() => {
    let alive = true;
    setArt({ mark: null, hero: null });
    void worldArtwork(mount).then((answer) => { if (alive) setArt(answer); });
    return () => { alive = false; };
  }, [mount]);
  return art;
}

function WorldSection({ label, rows, view, showing, onSelect }: {
  label?: string; rows: LibraryWorld[]; view: ClientView; showing: LibraryWorld | null; onSelect(key: string): void;
}) {
  return <section className="world-section">
    {label !== undefined && <h2>{label}</h2>}
    {rows.map((world) => <span className="row-tip" key={world.key} title={`${world.displayName} — ${lifecycle(view, world)}`}>
      <Button className="world-row" data-selected={world.key === showing?.key || undefined}
        onPress={() => onSelect(world.key)} aria-label={`${world.displayName} — ${lifecycle(view, world)}`}>
        <WorldMark world={world} />
        <span>{world.displayName}</span>
      </Button>
    </span>)}
  </section>;
}

/**
 * A World's mark: its own artwork where it ships one, and a plate cut from
 * its accent where it does not. The fallback is not a placeholder — a World
 * that ships no art is making a choice rather than missing a file.
 */
function WorldMark({ world }: { world: LibraryWorld }) {
  const art = useWorldArtwork(world.worldMount);
  if (art.mark !== null) return <img className="world-mark" src={art.mark} alt="" />;
  return <span className="world-mark" style={{ background: accentColor(world) }}>{world.worldMount.slice(0, 1).toUpperCase()}</span>;
}

function WorldDetail({ view, world, dispatch, dark }: { view: ClientView; world: LibraryWorld; dispatch(action: ClientAction): Promise<void>; dark: boolean }) {
  const entryPath = world.opensAt;
  const opening = isOpening(view, world);
  const serving = servingWorld(view, world.worldMount);
  const live = serving.filter((head) => head.state === "running");
  const running = !opening && live.length > 0;
  const stoppable = serving.find((head) => head.owned);
  const stopping = stoppable !== undefined && view.inFlight.includes(actionKey.stopHead(stoppable.id));
  const update = world.update;
  // Consent returns in one IPC turn; the fetch and migration run on the
  // daemon afterwards. The control stays pending through both, or a person
  // watches "UPDATE" flash and reads the update as done before it started.
  const updating = view.inFlight.includes(actionKey.updateWorld(world.worldMount))
    || updateInProgress(update);
  const state = lifecycle(view, world);
  // What became of the last update, when it did not simply land: a bundle
  // this build cannot run, or a refused operation. Said in place — a person
  // who pressed UPDATE and got silence learns to distrust the control.
  const updateNote = update === null ? null
    : update.unmet !== null && update.unmet.length > 0
      ? `The newest bundle${update.available === null ? "" : ` (${update.available})`} cannot run on this build: ${update.unmet.join("; ")}`
      : update.phase === "refused"
        ? `The last update was refused: ${update.message ?? "no reason was recorded"}`
        : null;
  return <section className="world-detail">
    <WorldHero world={world} />
    <div className="world-action-band">
      {stopping ? <PendingAction label="STOPPING" />
        : running ? <RunningAction owned={stoppable !== undefined}
          onOpen={entryPath === null ? undefined : () => void dispatch({ type: "open", world: world.worldMount, entryPath })}
          onStop={() => { if (stoppable !== undefined) void dispatch({ type: "stopHead", id: stoppable.id }); }} />
        : opening ? <PendingAction label="LAUNCHING" />
        : updating ? <PendingAction label="UPDATING" />
        : update?.behind ? <span className="tip" title={update.available === null
            ? "Fetch the newest bundle for this World"
            : `Update to ${update.available} — this device is serving ${update.serving ?? "the built-in version"}`}>
          <Button className="update-control" aria-label={`Update ${world.displayName}`}
            onPress={() => void dispatch({ type: "updateWorld", world: world.worldMount })}>↻ <span>UPDATE</span></Button></span>
        : state === "Ready" || state === "Stopped" ? <span className="tip" title={state === "Stopped" ? "This World's head exited — start it again" : "Start this World and hand it to my browser"}>
          <Button className="launch-control" aria-label="Launch World" onPress={() => {
            if (world.opensAt !== null) void dispatch({ type: "open", world: world.worldMount, entryPath: world.opensAt });
          }}>▶ <span>LAUNCH</span></Button></span>
        : <div className="lifecycle-state">ⓘ {state}</div>}
      <span className="tip world-settings-tip" title="World settings">
        <Button className="world-settings" aria-label={`${world.displayName} settings`} onPress={() => void summonWorldSettings({
          key: world.key,
          name: world.displayName,
          worldMount: world.worldMount,
          entryPath: world.opensAt,
          version: world.version,
          activeOrigin: (live[0] ?? serving[0])?.origin ?? null,
          dark,
        })}>⚙</Button>
      </span>
    </div>
    <div className="world-detail-content">
      {updating && update?.progress != null && <p className="update-note">{update.progress}</p>}
      {!updating && updateNote !== null && <p className="update-note">{updateNote}</p>}
      <PeopleGlance people={world.people} />
    </div>
  </section>;
}

/**
 * The World's own frame, where it ships one. The accent gradient stays either
 * way and goes over the top of it as a scrim: translucent where there is art
 * to show through, opaque where there is none — the artless case draws
 * exactly what it drew before.
 */
function WorldHero({ world }: { world: LibraryWorld }) {
  const art = useWorldArtwork(world.worldMount);
  const accent = accentColor(world);
  const wash = `color-mix(in srgb, ${accent} 38%, #11151d)`;
  const style: React.CSSProperties & { "--world-accent": string } = { height: heroHeight, "--world-accent": accent };
  if (art.hero !== null) {
    style.background = `linear-gradient(135deg, color-mix(in srgb, ${accent} 72%, transparent), `
      + `color-mix(in srgb, ${wash} 88%, transparent)), url("${art.hero}") center / cover`;
  }
  return <div className="world-hero" style={style}><h1>{world.displayName}</h1></div>;
}

/**
 * The book ∩ this Space, at a glance. Every row resolves through the book
 * (the face, the name, the AI mark) with presence measured in this Space
 * alone, and the two absences stay apart: an unread book and a Space nobody
 * in the book is addressed in say different things.
 */
function PeopleGlance({ people }: { people: WorldPerson[] | null }) {
  if (people === null) return <div className="glance-card">The book has not been read.</div>;
  if (people.length === 0) return <div className="glance-card">Nobody in the book is addressed here.</div>;
  const { here, holding } = glanceTiers(people);
  return <div className="glance-card people-glance">
    {here.length > 0 && <>
      <span className="glance-tier">{here.length === 1 ? "1 is in the World now" : `${here.length} are in the World now`}</span>
      {here.map((person) => <PersonTile key={person.name} name={person.name} picture={person.picture}
        presence={person.presence} agent={person.agent} size={32} />)}
    </>}
    {holding.length > 0 && <>
      <span className="glance-tier">{holding.length === 1 ? "1 has it in their library" : `${holding.length} have it in their library`}</span>
      <div className="glance-faces">
        {holding.map((person) => {
          const label = presenceLabel(person.presence) === null
            ? person.name
            : `${person.name} — ${presenceLabel(person.presence)}`;
          return <span key={person.name} title={label} role="img" aria-label={label}
            style={{ opacity: person.presence === "offline" ? 0.45 : person.presence === "away" ? 0.7 : 1 }}>
            <FacePlate picture={person.picture} name={person.name} size={28} />
          </span>;
        })}
      </div>
    </>}
  </div>;
}

function RunningAction({ owned, onOpen, onStop }: { owned: boolean; onOpen?: () => void; onStop(): void }) {
  return <div className="running-control">
    {owned && <span className="tip" title="Stop the head serving your Worlds">
      <Button className="stop-control" aria-label="Stop the head" onPress={onStop}>× <span>STOP</span></Button></span>}
    <span className="tip" title={onOpen === undefined ? "This World has not declared where to open it." : "Take me to the running World"}>
      <Button className="open-control" aria-label="Go to running World" onPress={onOpen} isDisabled={onOpen === undefined}>
        {owned ? "↗" : <><span>OPEN</span> ↗</>}</Button></span>
  </div>;
}

function PendingAction({ label }: { label: string }) { return <div className="pending-control"><span className="spinner" />{label}</div>; }
function LoadingLibrary() { return <section className="library loading-library"><aside className="library-rail" style={{ width: railWidth }}><div className="skeleton heading" /><div className="skeleton row" /><div className="skeleton row" /></aside><div className="skeleton hero" /></section>; }
function EmptyLibrary() { return <section className="empty-library"><h1>Library</h1><p>This build installs no Worlds.</p><p>A World ships inside the client. This binary was built without any, so there is nothing to open.</p></section>; }

/**
 * Persistent operational truth at the bottom of the window — a status bar,
 * not a notification stack. What is true sits left of the rule; what can be
 * done sits right of it.
 */
function OperationalBar({ view, onSummonWindow, dispatch }: { view: ClientView; onSummonWindow(surface: OwnedWindowSurface): void; dispatch(action: ClientAction): Promise<void> }) {
  const status = identityStatus(view);
  const activity = activityLine(view);
  const spaces = view.host?.orbitCount ?? view.orbits.length;
  const rolling = view.inFlight.includes(actionKey.reload);
  return <footer className="operational-bar" aria-live={view.inFlight.length > 0 || view.failures.length > 0 ? "polite" : "off"}>
    <span className={`identity-status tone-${status.tone}`}>
      {view.loading ? <span className="spinner tiny" /> : <span className="status-icon">⌁</span>}{status.label}
    </span><span className="bar-divider" />
    <span className="activity">{activity}</span>
    <UpdateAffordance update={view.update} />
    {rolling ? <span className="roll-forward rolling"><span className="spinner tiny" /> Rolling forward…</span>
      : view.image?.sourceChanged === true && <span className="tip" title="A newer build is on disk — restart everything onto it">
        <Button className="roll-forward" aria-label="Roll forward onto the new build"
          onPress={() => void dispatch({ type: "reload" })}>⟳ Roll forward</Button></span>}
    <span>{view.heads.length} {view.heads.length === 1 ? "head" : "heads"}</span>
    <span className="bar-spaces">{spaces} {spaces === 1 ? "Space" : "Spaces"}</span>
    {view.host !== null && <code className="bar-version">v{view.host.version}</code>}
    <span className="bar-divider" />
    <span className="tip" title="Coordinate displays">
      <Button className="bar-icon" aria-label="Displays" onPress={() => void onSummonWindow("displays")}>⌁</Button></span>
    <span className="tip" title="Open the address book">
      <Button className="bar-icon" aria-label="Address book" onPress={() => void onSummonWindow("book")}>◉</Button></span>
  </footer>;
}

/**
 * This client's own updating, which is never a choice about whether to take
 * one (CLIENT-47). The client is evergreen: staging is silent and continuous,
 * and a release applies at the next natural boundary. The only machine that
 * needs asking is the one that never reaches a boundary — a session left open
 * for days — so this is a request to restart, escalating with how long the
 * release has waited, and nothing at all in the ordinary case.
 *
 * `attention` is deliberately not an update: a signature that did not verify
 * or a pointer that went backwards means somebody should look, and folding
 * either into silence is exactly the quiet an attack buys.
 */
function UpdateAffordance({ update }: { update: UpdateIntent | null }) {
  // The floor is the one case that moves on its own: once declared work has
  // drained there is nothing to ask, so the restart is taken. Until then it
  // says what it is waiting for rather than going quiet.
  useEffect(() => {
    if (update?.kind === "forced" && update.holding.length === 0) {
      void restartForUpdate(update.version);
    }
  }, [update]);

  if (update === null) return null;
  if (update.kind === "forced") {
    return <span className="tip" title={`${update.version} is required; this build is below the published floor`}>
      <span className="update-forced" role="status">
        <span className="spinner tiny" />
        {update.holding.length === 0
          ? "Restarting to update…"
          : `Update required — finishing ${update.holding.length === 1 ? "1 task" : `${update.holding.length} tasks`}`}
      </span></span>;
  }
  if (update.kind === "attention") {
    return <span className="tip" title={update.why}>
      <span className="update-attention" role="status">⚠ Update needs attention</span></span>;
  }
  if (update.kind === "waiting") {
    return <span className="tip" title={`${update.version} is ready; waiting for ${update.holding.join(", ")}`}>
      <span className="update-waiting" role="status">
        <span className="spinner tiny" /> Update waits for {update.holding.length === 1 ? "1 task" : `${update.holding.length} tasks`}
      </span></span>;
  }
  return <span className="tip" title={`${update.version} is staged and applies when this client restarts`}>
    <Button className={`update-restart urgency-${update.urgency}`} aria-label={`Restart to update to ${update.version}`}
      onPress={() => void restartForUpdate(update.version)}>↻ Restart to update</Button></span>;
}

/// The heads serving exactly this World. A head with `world: null` predates
/// the pin and deliberately matches no row: a surface cannot say a definite
/// thing about it, and saying an indefinite thing is the defect the field
/// closes. Opening Issues must never paint Signage as Running.
export function servingWorld(view: ClientView, mount: string): Head[] {
  return view.heads.filter((head) => head.orbit === null && head.world === mount);
}
/// Read from the head's own state, never from the fact that a row exists.
/// Exited heads stay listed so a person can see the thing they opened died,
/// so counting rows paints a crashed World as Running.
export function lifecycle(view: ClientView, world: LibraryWorld): "Launching" | "Running" | "Ready" | "Unavailable" | "Stopped" | "Unknown" {
  if (isOpening(view, world)) return "Launching";
  if (world.opensAt === null) return "Unavailable";
  const heads = servingWorld(view, world.worldMount);
  if (heads.length === 0) return "Ready";
  if (heads.some((head) => head.state === "running")) return "Running";
  // `unknown` outranks `exited`: a head nobody could poll may still be
  // serving, and "Stopped" would be the same confident guess the other way.
  if (heads.some((head) => head.state === "unknown")) return "Unknown";
  return "Stopped";
}
function isOpening(view: ClientView, world: LibraryWorld): boolean {
  return world.opensAt !== null && view.inFlight.includes(actionKey.open(world.worldMount));
}
function accentColor(world: LibraryWorld): string {
  return world.accent === null ? "var(--surface-500)" : `#${world.accent.toString(16).padStart(6, "0")}`;
}
