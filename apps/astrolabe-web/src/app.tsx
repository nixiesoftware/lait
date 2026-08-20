import { useCallback, useEffect, useMemo, useState } from "react";
import { Button, Menu, MenuItem, MenuTrigger, Popover, Separator } from "react-aria-components";

import {
  actionKey,
  createClientTransport,
  keyFor,
  loadingClientView,
  type ClientAction,
  type ClientTransport,
  type ClientView,
  type LibraryWorld,
} from "./client";
import { BookSurface, DisplaysSurface, RecordSurface, type SecondarySurface } from "./lifecycle";
import { resolvePlatform, type PlatformProfile } from "./platform";

const utilityBarHeight = 32;
const railWidth = 224;
const heroHeight = 196;

export function App() {
  const [platform] = useState<PlatformProfile>(() => resolvePlatform());
  const [dark, setDark] = useState(true);
  const transport = useMemo(() => createClientTransport(), []);
  const { view, dispatch } = useClient(transport);
  const [selected, setSelected] = useState<string | null>(null);
  const [surface, setSurface] = useState<SecondarySurface | null>(null);

  useEffect(() => { document.documentElement.dataset.platform = platform; }, [platform]);
  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      if (event.key === "F5") {
        event.preventDefault();
        void dispatch({ type: "refresh" });
      }
      if ((event.metaKey || event.ctrlKey) && event.shiftKey) {
        const target = event.key.toLowerCase() === "b" ? "book" : event.key.toLowerCase() === "d" ? "displays" : event.key.toLowerCase() === "r" ? "record" : null;
        if (target !== null) { event.preventDefault(); setSurface(target); }
      }
    };
    window.addEventListener("keydown", shortcut);
    return () => window.removeEventListener("keydown", shortcut);
  }, [dispatch]);

  const worlds = view.library;
  const showing = useMemo(
    () => worlds?.find((world) => world.key === selected) ?? worlds?.[0] ?? null,
    [selected, worlds],
  );

  return <main className="page" data-theme={dark ? "dark" : "light"}>
    <section className="astrolabe-window" aria-label="Astrolabe">
      <Caption platform={platform} dark={dark} setDark={setDark} onShowSurface={setSurface}
        refreshing={view.inFlight.includes(actionKey.refresh)} onRefresh={() => void dispatch({ type: "refresh" })} />
      <div className="client-body">
        {surface === null && <Library view={view} showing={showing} onSelect={setSelected} dispatch={dispatch} />}
        {surface === "book" && <BookSurface view={view} dispatch={dispatch} onBack={() => setSurface(null)} />}
        {surface === "displays" && <DisplaysSurface view={view} dispatch={dispatch} onBack={() => setSurface(null)} />}
        {surface === "record" && <RecordSurface view={view} dispatch={dispatch} onBack={() => setSurface(null)} />}
        <OperationalBar view={view} onShowSurface={setSurface} />
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

function Caption({ platform, dark, setDark, refreshing, onRefresh, onShowSurface }: {
  platform: PlatformProfile; dark: boolean; setDark(next: boolean): void; refreshing: boolean; onRefresh(): void; onShowSurface(surface: SecondarySurface): void;
}) {
  const systemMenu = platform === "macos";
  return <header className="caption" data-tauri-drag-region style={{ height: utilityBarHeight }}>
    {systemMenu ? <div className="traffic-light-clearance" /> : <MenuTrigger>
      <Button className="wordmark" aria-label="Astrolabe settings">ASTROLABE</Button>
      <Popover className="settings-popover"><Menu className="settings-menu" aria-label="Astrolabe settings">
        <header className="settings-header"><strong>ASTROLABE</strong></header><Separator />
        <span className="settings-section">CLIENT SETTINGS</span>
        <MenuItem id="book" onAction={() => onShowSurface("book")}>Address book <kbd>⌘⇧B</kbd></MenuItem>
        <MenuItem id="displays" onAction={() => onShowSurface("displays")}>Displays <kbd>⌘⇧D</kbd></MenuItem>
        <MenuItem id="record" onAction={() => onShowSurface("record")}>Local record <kbd>⌘⇧R</kbd></MenuItem>
        <MenuItem id="refresh" isDisabled={refreshing} onAction={onRefresh}>Refresh local state <kbd>F5</kbd></MenuItem>
        <MenuItem id="theme" onAction={() => setDark(!dark)}>{dark ? "Use light theme" : "Use dark theme"}</MenuItem>
      </Menu></Popover>
    </MenuTrigger>}
    <div className="caption-drag" />
    {!systemMenu && <div className="caption-controls" aria-label="Window controls">
      <Button className="caption-button" aria-label="Minimise"><span className="minimise-mark" /></Button>
      <Button className="caption-button close" aria-label="Close (it keeps serving in the tray)"><span className="close-mark" /></Button>
    </div>}
  </header>;
}

function Library({ view, showing, onSelect, dispatch }: {
  view: ClientView;
  showing: LibraryWorld | null;
  onSelect(key: string): void;
  dispatch(action: ClientAction): Promise<void>;
}) {
  if (view.library === null) return <LoadingLibrary />;
  if (view.library.length === 0) return <EmptyLibrary />;
  const running = view.library.filter((world) => isRunning(view));
  const ready = view.library.filter((world) => !isRunning(view) && world.opensAt !== null);
  const unavailable = view.library.filter((world) => !isRunning(view) && world.opensAt === null);
  return <section className="library">
    <aside className="library-rail" style={{ width: railWidth }}>
      <div className="library-heading"><span>LIBRARY</span><span>{view.library.length}</span></div>
      <div className="world-sections">
        {running.length > 0 && <WorldSection label="RUNNING" rows={running} view={view} showing={showing} onSelect={onSelect} />}
        {ready.length > 0 && <WorldSection rows={ready} view={view} showing={showing} onSelect={onSelect} />}
        {unavailable.length > 0 && <WorldSection label="UNAVAILABLE" rows={unavailable} view={view} showing={showing} onSelect={onSelect} />}
      </div>
    </aside>
    {showing !== null && <WorldDetail view={view} world={showing} dispatch={dispatch} />}
  </section>;
}

function WorldSection({ label, rows, view, showing, onSelect }: {
  label?: string; rows: LibraryWorld[]; view: ClientView; showing: LibraryWorld | null; onSelect(key: string): void;
}) {
  return <section className="world-section">
    {label !== undefined && <h2>{label}</h2>}
    {rows.map((world) => <Button key={world.key} className="world-row" data-selected={world.key === showing?.key || undefined}
      onPress={() => onSelect(world.key)} aria-label={`${world.displayName} — ${lifecycle(view, world)}`}>
      <span className="world-mark" style={{ background: accentColor(world) }}>{world.worldMount.slice(0, 1).toUpperCase()}</span>
      <span>{world.displayName}</span>
    </Button>)}
  </section>;
}

function WorldDetail({ view, world, dispatch }: { view: ClientView; world: LibraryWorld; dispatch(action: ClientAction): Promise<void> }) {
  const entryPath = world.opensAt;
  const opening = entryPath !== null && view.inFlight.includes(actionKey.open(entryPath));
  const serving = view.heads.filter((head) => head.orbit === null);
  const running = !opening && serving.length > 0;
  const stoppable = serving.find((head) => head.owned);
  const stopping = stoppable !== undefined && view.inFlight.includes(actionKey.stopHead(stoppable.id));
  const updating = view.inFlight.includes(actionKey.updateWorld(world.worldMount));
  const state = lifecycle(view, world);
  return <section className="world-detail">
    <div className="world-hero" style={{ height: heroHeight, "--world-accent": accentColor(world) } as React.CSSProperties}><h1>{world.displayName}</h1></div>
    <div className="world-action-band">
      {stopping ? <PendingAction label="STOPPING" />
        : running ? <RunningAction owned={stoppable !== undefined}
          onOpen={entryPath === null ? undefined : () => void dispatch({ type: "open", entryPath })}
          onStop={() => { if (stoppable !== undefined) void dispatch({ type: "stopHead", id: stoppable.id }); }} />
        : opening ? <PendingAction label="LAUNCHING" />
        : updating ? <PendingAction label="UPDATING" />
        : world.update?.behind ? <Button className="update-control" aria-label={`Update ${world.displayName}`} onPress={() => void dispatch({ type: "updateWorld", world: world.worldMount })}>↻ <span>UPDATE</span></Button>
        : state === "Ready" ? <Button className="launch-control" aria-label="Launch World" onPress={() => {
          if (world.opensAt !== null) void dispatch({ type: "open", entryPath: world.opensAt });
        }}>▶ <span>LAUNCH</span></Button> : <div className="lifecycle-state">ⓘ {state}</div>}
      <Button className="world-settings" aria-label={`${world.displayName} settings`}>⚙</Button>
    </div>
    <div className="world-detail-content"><div className="glance-card">
      {world.people === null ? "The book has not been read." : "Nobody in the book is addressed here."}
    </div></div>
  </section>;
}

function RunningAction({ owned, onOpen, onStop }: { owned: boolean; onOpen?: () => void; onStop(): void }) {
  return <div className="running-control">
    {owned && <Button className="stop-control" aria-label="Stop the head" onPress={onStop}>× <span>STOP</span></Button>}
    <Button className="open-control" aria-label="Go to running World" onPress={onOpen} isDisabled={onOpen === undefined}>{owned ? "↗" : <><span>OPEN</span> ↗</>}</Button>
  </div>;
}

function PendingAction({ label }: { label: string }) { return <div className="pending-control"><span className="spinner" />{label}</div>; }
function LoadingLibrary() { return <section className="library loading-library"><aside className="library-rail" style={{ width: railWidth }}><div className="skeleton heading" /><div className="skeleton row" /><div className="skeleton row" /></aside><div className="skeleton hero" /></section>; }
function EmptyLibrary() { return <section className="empty-library"><h1>Library</h1><p>This build installs no Worlds.</p><p>A World ships inside the client. This binary was built without any, so there is nothing to open.</p></section>; }

function OperationalBar({ view, onShowSurface }: { view: ClientView; onShowSurface(surface: SecondarySurface): void }) {
  const status = view.loading ? "Connecting to local identity" : view.failures.length > 0 ? "Needs attention" : view.stale ? "Local identity degraded" : view.host === null ? "Local identity unavailable" : "Local identity online";
  const activity = view.inFlight.includes(actionKey.refresh) ? "Reading local state…" : view.notices[0]?.said ?? "All local systems current";
  const spaces = view.host?.orbitCount ?? 0;
  return <footer className="operational-bar" aria-live={view.inFlight.length > 0 || view.failures.length > 0 ? "polite" : "off"}>
    <span className="identity-status"><span className="status-icon">⌁</span>{status}</span><span className="bar-divider" />
    <span className="activity">{activity}</span><span>{view.heads.length} {view.heads.length === 1 ? "head" : "heads"}</span>
    <span>{spaces} {spaces === 1 ? "Space" : "Spaces"}</span>{view.host !== null && <code>v{view.host.version}</code>}
    <span className="bar-divider" /><Button className="bar-icon" aria-label="Displays" onPress={() => onShowSurface("displays")}>⌁</Button><Button className="bar-icon" aria-label="Address book" onPress={() => onShowSurface("book")}>◉</Button><Button className="bar-icon" aria-label="Local record" onPress={() => onShowSurface("record")}>≡</Button>
  </footer>;
}

function lifecycle(view: ClientView, world: LibraryWorld): "Launching" | "Running" | "Ready" | "Unavailable" {
  if (world.opensAt !== null && view.inFlight.includes(actionKey.open(world.opensAt))) return "Launching";
  if (world.opensAt === null) return "Unavailable";
  return isRunning(view) ? "Running" : "Ready";
}
function isRunning(view: ClientView): boolean { return view.heads.some((head) => head.orbit === null); }
function accentColor(world: LibraryWorld): string {
  return world.accent === null ? "var(--surface-500)" : `#${world.accent.toString(16).padStart(6, "0")}`;
}
