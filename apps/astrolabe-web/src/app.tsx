import { useEffect, useMemo, useState } from "react";
import { Button, Menu, MenuItem, MenuTrigger, Popover, Separator } from "react-aria-components";

import { actionKey, cannedClientView, type ClientView, type LibraryWorld } from "./client";
import { resolvePlatform, type PlatformProfile } from "./platform";

const utilityBarHeight = 32;
const railWidth = 224;
const heroHeight = 196;

export function App() {
  const [platform] = useState<PlatformProfile>(() => resolvePlatform());
  const [dark, setDark] = useState(true);
  const [view, setView] = useState<ClientView>(cannedClientView);
  const [selected, setSelected] = useState<string | null>(null);

  useEffect(() => { document.documentElement.dataset.platform = platform; }, [platform]);
  useEffect(() => {
    const refresh = (event: KeyboardEvent) => {
      if (event.key !== "F5") return;
      event.preventDefault();
      void dispatchRefresh(setView);
    };
    window.addEventListener("keydown", refresh);
    return () => window.removeEventListener("keydown", refresh);
  }, []);

  const worlds = view.library;
  const showing = useMemo(
    () => worlds?.find((world) => world.key === selected) ?? worlds?.[0] ?? null,
    [selected, worlds],
  );

  return <main className="page" data-theme={dark ? "dark" : "light"}>
    <section className="astrolabe-window" aria-label="Astrolabe">
      <Caption platform={platform} dark={dark} setDark={setDark}
        refreshing={view.inFlight.includes(actionKey.refresh)} onRefresh={() => void dispatchRefresh(setView)} />
      <div className="client-body">
        <Library view={view} showing={showing} onSelect={setSelected} />
        <OperationalBar view={view} />
      </div>
    </section>
  </main>;
}

function Caption({ platform, dark, setDark, refreshing, onRefresh }: {
  platform: PlatformProfile; dark: boolean; setDark(next: boolean): void; refreshing: boolean; onRefresh(): void;
}) {
  const systemMenu = platform === "macos";
  return <header className="caption" style={{ height: utilityBarHeight }}>
    {systemMenu ? <div className="traffic-light-clearance" /> : <MenuTrigger>
      <Button className="wordmark" aria-label="Astrolabe settings">ASTROLABE</Button>
      <Popover className="settings-popover"><Menu className="settings-menu" aria-label="Astrolabe settings">
        <header className="settings-header"><strong>ASTROLABE</strong></header><Separator />
        <span className="settings-section">CLIENT SETTINGS</span>
        <MenuItem id="displays">Displays</MenuItem>
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

function Library({ view, showing, onSelect }: { view: ClientView; showing: LibraryWorld | null; onSelect(key: string): void }) {
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
    {showing !== null && <WorldDetail view={view} world={showing} />}
  </section>;
}

function WorldSection({ label, rows, view, showing, onSelect }: {
  label?: string; rows: LibraryWorld[]; view: ClientView; showing: LibraryWorld | null; onSelect(key: string): void;
}) {
  return <section className="world-section">
    {label !== undefined && <h2>{label}</h2>}
    {rows.map((world) => <Button key={world.key} className="world-row" data-selected={world.key === showing?.key || undefined}
      onPress={() => onSelect(world.key)} aria-label={`${world.displayName} — ${lifecycle(view, world)}`}>
      <span className="world-mark" style={{ background: world.accent ?? "var(--surface-500)" }}>{world.worldMount.slice(0, 1).toUpperCase()}</span>
      <span>{world.displayName}</span>
    </Button>)}
  </section>;
}

function WorldDetail({ view, world }: { view: ClientView; world: LibraryWorld }) {
  const [opening, setOpening] = useState(false);
  const state = lifecycle(view, world);
  const running = !opening && isRunning(view);
  return <section className="world-detail">
    <div className="world-hero" style={{ height: heroHeight, "--world-accent": world.accent ?? "#53667d" } as React.CSSProperties}><h1>{world.displayName}</h1></div>
    <div className="world-action-band">
      {opening ? <PendingAction label="LAUNCHING" /> : running ? <RunningAction owned={view.heads.some((head) => head.owned && head.orbit === null)} />
        : state === "Ready" ? <Button className="launch-control" aria-label="Launch World" onPress={() => {
          if (world.opensAt === null) return;
          setOpening(true); window.setTimeout(() => setOpening(false), 900);
        }}>▶ <span>LAUNCH</span></Button> : <div className="lifecycle-state">ⓘ {state}</div>}
      <Button className="world-settings" aria-label={`${world.displayName} settings`}>⚙</Button>
    </div>
    <div className="world-detail-content"><div className="glance-card">
      {world.people === null ? "The book has not been read." : "Nobody in the book is addressed here."}
    </div></div>
  </section>;
}

function RunningAction({ owned }: { owned: boolean }) {
  return <div className="running-control">
    {owned && <Button className="stop-control" aria-label="Stop the head">× <span>STOP</span></Button>}
    <Button className="open-control" aria-label="Go to running World">{owned ? "↗" : <><span>OPEN</span> ↗</>}</Button>
  </div>;
}

function PendingAction({ label }: { label: string }) { return <div className="pending-control"><span className="spinner" />{label}</div>; }
function LoadingLibrary() { return <section className="library loading-library"><aside className="library-rail" style={{ width: railWidth }}><div className="skeleton heading" /><div className="skeleton row" /><div className="skeleton row" /></aside><div className="skeleton hero" /></section>; }
function EmptyLibrary() { return <section className="empty-library"><h1>Library</h1><p>This build installs no Worlds.</p><p>A World ships inside the client. This binary was built without any, so there is nothing to open.</p></section>; }

function OperationalBar({ view }: { view: ClientView }) {
  const status = view.loading ? "Connecting to local identity" : view.failures.length > 0 ? "Needs attention" : view.stale ? "Local identity degraded" : view.host === null ? "Local identity unavailable" : "Local identity online";
  const activity = view.inFlight.includes(actionKey.refresh) ? "Reading local state…" : view.notices[0] ?? "All local systems current";
  const spaces = view.host?.orbitCount ?? 0;
  return <footer className="operational-bar" aria-live={view.inFlight.length > 0 || view.failures.length > 0 ? "polite" : "off"}>
    <span className="identity-status"><span className="status-icon">⌁</span>{status}</span><span className="bar-divider" />
    <span className="activity">{activity}</span><span>{view.heads.length} {view.heads.length === 1 ? "head" : "heads"}</span>
    <span>{spaces} {spaces === 1 ? "Space" : "Spaces"}</span>{view.host !== null && <code>v{view.host.version}</code>}
    <span className="bar-divider" /><Button className="bar-icon" aria-label="Displays">⌁</Button><Button className="bar-icon" aria-label="Address book">◉</Button>
  </footer>;
}

function lifecycle(view: ClientView, world: LibraryWorld): "Launching" | "Running" | "Ready" | "Unavailable" {
  if (world.opensAt !== null && view.inFlight.includes(actionKey.open(world.opensAt))) return "Launching";
  if (world.opensAt === null) return "Unavailable";
  return isRunning(view) ? "Running" : "Ready";
}
function isRunning(view: ClientView): boolean { return view.heads.some((head) => head.orbit === null); }
async function dispatchRefresh(setView: (recipe: (current: ClientView) => ClientView) => void): Promise<void> {
  setView((current) => ({ ...current, inFlight: [...current.inFlight, actionKey.refresh] }));
  await new Promise((resolve) => window.setTimeout(resolve, 500));
  setView((current) => ({ ...current, inFlight: current.inFlight.filter((key) => key !== actionKey.refresh) }));
}
