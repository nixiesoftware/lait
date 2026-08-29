/**
 * Big Picture: this machine as a screen.
 *
 * The *member* profile of REACH's two-profile split. Astrolabe already holds
 * the Space these pixels came from, so nothing here pairs, enrols, or carries
 * a credential — the surface asks the daemon to render one exact World
 * surface and draws what comes back. Leaving is always available, and
 * revocation needs no message: losing standing simply stops the answer.
 */
import { useEffect, useMemo, useState } from "react";

import {
  hostOwnsFullscreen,
  setFullscreen,
  type ClientAction,
  type ClientView,
  type DisplaySurface,
  type PresentationFacts,
  type PresentedItem,
  type PresentedProgram,
} from "./client";
import { inputPrompt, surfaceInput } from "./displays";

type Dispatch = (action: ClientAction) => Promise<void>;

/** How long an item with no declared duration holds the screen. */
export const untimedHoldMs = 15_000;

/**
 * The floor on re-asking, whatever a program declares. A surface that asked
 * for a one-millisecond refresh would otherwise spend the machine on it.
 */
export const refreshFloorMs = 5_000;

export function holdMs(item: PresentedItem): number {
  return item.durationMs ?? untimedHoldMs;
}

export function refreshDelayMs(program: PresentedProgram): number | null {
  if (program.refreshAfterMs === null) return null;
  return Math.max(program.refreshAfterMs, refreshFloorMs);
}

export type Advance =
  | { kind: "show"; index: number }
  | { kind: "refresh" }
  | { kind: "blank" }
  | { kind: "hold" };

/**
 * Where a finished item sends the screen. An unknown cycle holds the last
 * frame — the conservative reading; blanking on one would discard a program
 * because this build did not recognise a word.
 */
export function advance(cycle: string, index: number, length: number): Advance {
  if (index < length - 1) return { kind: "show", index: index + 1 };
  switch (cycle) {
    case "loop": return { kind: "show", index: 0 };
    case "poll_at_end": return { kind: "refresh" };
    case "blank_at_end": return { kind: "blank" };
    default: return { kind: "hold" };
  }
}

/**
 * Whether the page holds the display, watched. The desktop host answers
 * `null`: fullscreen there is a window fact the page can neither lose nor
 * need to retake. A browser can take it back at any time — its own Escape
 * never reaches this page — so the surface tracks the grant and keeps a
 * retake control on screen while it is missing.
 */
function useBrowserFullscreen(): boolean | null {
  const [held, setHeld] = useState<boolean | null>(
    () => hostOwnsFullscreen() ? null : document.fullscreenElement !== null,
  );
  useEffect(() => {
    if (hostOwnsFullscreen()) return;
    const update = () => setHeld(document.fullscreenElement !== null);
    document.addEventListener("fullscreenchange", update);
    return () => document.removeEventListener("fullscreenchange", update);
  }, []);
  return held;
}

/**
 * A screen must not sleep mid-program. Held while presenting, and re-asked
 * when the tab becomes visible again — the browser releases the lock on
 * every hide. Absence of the API is a quiet no: the surface still draws.
 */
function useScreenWakeLock(): void {
  useEffect(() => {
    let lock: WakeLockSentinel | null = null;
    let alive = true;
    const acquire = async () => {
      if (!("wakeLock" in navigator) || document.visibilityState !== "visible") return;
      try {
        const sentinel = await navigator.wakeLock.request("screen");
        if (alive) lock = sentinel;
        else void sentinel.release();
      } catch {
        // Power settings or policy said no; the program plays regardless.
      }
    };
    const onVisible = () => { void acquire(); };
    void acquire();
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      alive = false;
      document.removeEventListener("visibilitychange", onVisible);
      void lock?.release();
    };
  }, []);
}

export function BigPictureSurface({ presentation, view, dispatch }: {
  presentation: PresentationFacts; view: ClientView; dispatch: Dispatch;
}) {
  const fullscreen = useBrowserFullscreen();
  useScreenWakeLock();
  // The shown item, with a nonce so a loop back onto the same index still
  // re-arms its hold timer.
  const [shown, setShown] = useState({ index: 0, nonce: 0 });
  const program = presentation.program;
  const items = program?.items ?? [];

  // Take the display, not the work area — and give it back on the way out,
  // whatever the reason for leaving. A client that exited Big Picture and
  // kept the screen would be a window nobody could get behind.
  useEffect(() => {
    void setFullscreen(true);
    return () => { void setFullscreen(false); };
  }, []);

  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if (event.key === "Escape") void dispatch({ type: "leavePresentation" });
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [dispatch]);

  // A new revision restarts the program; a re-ask that returned the same
  // thing must not, or a long item would never finish.
  const programKey = useMemo(() => JSON.stringify(program), [program]);
  useEffect(() => { setShown({ index: 0, nonce: 0 }); }, [programKey]);

  useEffect(() => {
    if (program === null || shown.index >= items.length) return;
    const timer = window.setTimeout(() => {
      const next = advance(program.cycle, shown.index, items.length);
      if (next.kind === "show") setShown((current) => ({ index: next.index, nonce: current.nonce + 1 }));
      else if (next.kind === "refresh") void dispatch({ type: "presentRefresh" });
      else if (next.kind === "blank") setShown((current) => ({ index: items.length, nonce: current.nonce }));
    }, holdMs(items[shown.index]));
    return () => window.clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- programKey stands in for program/items identity
  }, [programKey, shown, dispatch]);

  useEffect(() => {
    if (program === null) return;
    const delay = refreshDelayMs(program);
    if (delay === null) return;
    const timer = window.setTimeout(() => void dispatch({ type: "presentRefresh" }), delay);
    return () => window.clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- programKey stands in for program identity
  }, [programKey, dispatch]);

  // A television power-cycles weekly and a desktop monitor is unplugged
  // mid-meeting; both come back as a resize. Re-asking on that change is what
  // makes the render match the glass rather than the glass it was asked for.
  useEffect(() => {
    let frame = 0;
    const onResize = () => {
      window.cancelAnimationFrame(frame);
      frame = window.requestAnimationFrame(() => void dispatch({ type: "presentRefresh" }));
    };
    window.addEventListener("resize", onResize);
    return () => { window.removeEventListener("resize", onResize); window.cancelAnimationFrame(frame); };
  }, [dispatch]);

  const item = shown.index < items.length ? items[shown.index] : null;
  return <section className="big-picture" data-choosing={presentation.chosen === null || undefined}
    data-windowed={fullscreen === false || undefined} aria-label="Big Picture">
    {presentation.chosen === null
      // Entered and not yet pointed at anything. A real state, and the one
      // the person is in the instant they press the control.
      ? <PresentationChooser view={view} dispatch={dispatch} />
      : <>
        <Scene item={item} empty={program !== null && items.length === 0} />
        <PresentationChrome presentation={presentation} fullscreen={fullscreen} />
      </>}
    {presentation.chosen === null && <RetakeFullscreen fullscreen={fullscreen} corner />}
  </section>;
}

/**
 * The browser's way back to the display. Its own Escape exits fullscreen
 * without ever reaching this page, and a grant can only be re-asked from a
 * fresh gesture — so while the display is lost, this control stays on the
 * screen offering exactly that gesture. The desktop host never draws it.
 */
function RetakeFullscreen({ fullscreen, corner = false }: { fullscreen: boolean | null; corner?: boolean }) {
  if (fullscreen !== false) return null;
  return <button className={corner ? "present-retake corner" : "present-retake"}
    onClick={() => void setFullscreen(true)}>⛶ Fullscreen</button>;
}

/** What the current item draws, or an honest statement of why it does not. */
function Scene({ item, empty }: { item: PresentedItem | null; empty: boolean }) {
  if (item === null) {
    // The two absences are different facts and are drawn as such.
    return <Said headline={empty ? "This program has no items" : "Nothing to show"}
      detail={empty ? "The surface answered, and its program is empty." : "Waiting for the first render."} />;
  }
  switch (item.scene.kind) {
    case "frame":
      return <img className="presented-frame" src={item.scene.uri} alt={item.spokenSummary ?? ""} />;
    case "blank":
      return <Said
        headline={item.scene.reason === "source_unavailable" ? "This source is unavailable"
          : item.scene.reason === "program_ended" ? "The program has ended"
          : "Nothing to show"}
        detail="Blank was what the program asked for." />;
    case "unsupported":
      return <Said headline={`This screen cannot draw ${item.scene.output}`}
        detail="Live media is served by a display coordinator to a paired receiver. Astrolabe as a screen draws frames." />;
  }
}

/**
 * Text on the presentation ground. Colours are stated rather than taken from
 * the theme: this surface is always black — it is a screen, not a page.
 */
function Said({ headline, detail }: { headline: string; detail: string }) {
  return <div className="present-said"><strong>{headline}</strong><small>{detail}</small></div>;
}

/**
 * Receiver-native chrome, and the rule it follows: product pixels may not
 * suppress trust, source-state or delivery-state treatment. A screen that
 * could be made to look current while it is stale is the false-assurance
 * defect wherever it runs — so this draws over the frame, and only when
 * there is something to say.
 */
function PresentationChrome({ presentation, fullscreen }: { presentation: PresentationFacts; fullscreen: boolean | null }) {
  const program = presentation.program;
  const assessment = program?.assessment ?? null;
  const degraded = assessment !== null && assessment !== "current";
  return <div className="present-chrome">
    <div className="present-title-row">
      <span>{presentation.chosen?.title ?? ""}</span>
      <RetakeFullscreen fullscreen={fullscreen} />
      <span className="present-hint">Esc to leave</span>
    </div>
    <div className="present-banners">
      {degraded && <p className="present-banner">
        {assessment === "unavailable"
          ? "This source is unavailable. Showing what was last verified."
          : `This source is partial: ${program?.partialReasons.join(", ")}.`}
      </p>}
      {/* Delivery is a separate state from source truth, and it says so. */}
      {presentation.failure !== null && <p className="present-banner">Could not refresh: {presentation.failure}</p>}
    </div>
  </div>;
}

/**
 * Point this screen at something, from inside the mode. A surface rather
 * than a dialog: entering Big Picture is one press, so the choosing that
 * follows happens on the screen the person just made.
 */
function PresentationChooser({ view, dispatch }: { view: ClientView; dispatch: Dispatch }) {
  const display = view.display;
  const surfaces = display?.surfaces ?? [];
  const [orbit, setOrbit] = useState<string | null>(null);
  const [surfaceKey, setSurfaceKey] = useState<string | null>(null);
  const [input, setInput] = useState("");

  // Each absence says which kind it is. A coordinator that has not answered
  // is a read that has not happened; one that answered with no surfaces is a
  // selected World declaration that ships none.
  const blocked = display === null
    ? "The display coordinator has not answered yet."
    : surfaces.length === 0
      ? "The selected Worlds declare no display surface."
      : view.orbits.length === 0
        ? "This identity has no Orbit to draw from."
        : null;
  if (blocked !== null) {
    return <div className="present-center"><Said headline="Nothing to show here" detail={blocked} /></div>;
  }

  const chosenOrbit = orbit ?? view.orbits[0].space;
  const chosen = surfaces.find((surface) => `${surface.world}/${surface.surface}` === surfaceKey) ?? surfaces[0];
  const ready = input.trim() !== "";
  const show = () => {
    if (!ready) return;
    void dispatch({
      type: "presentHere",
      orbit: chosenOrbit,
      world: chosen.world,
      surface: chosen.surface,
      // A Signage screen id or an Issues project key is typed bare and
      // wrapped the way that surface's contract spells it; every other
      // surface takes its package's own JSON verbatim. The daemon hands
      // whatever this is to the package's canonicalizer, which is the only
      // thing entitled to judge it.
      input: surfaceInput(chosen, input.trim()),
      title: chosen.title,
    });
  };

  return <div className="present-center">
    <div className="present-chooser">
      <h1>What should this screen show?</h1>
      <label>ORBIT<select value={chosenOrbit} onChange={(event) => setOrbit(event.target.value)}>
        {view.orbits.map((row) => <option key={row.space} value={row.space}>{row.name}</option>)}
      </select></label>
      <label>DISPLAY SURFACE<select value={`${chosen.world}/${chosen.surface}`} onChange={(event) => {
        setSurfaceKey(event.target.value);
        setInput("");
      }}>
        {surfaces.map((surface: DisplaySurface) => <option key={`${surface.world}/${surface.surface}`}
          value={`${surface.world}/${surface.surface}`}>{surface.title} · {surface.world}</option>)}
      </select></label>
      {inputPrompt(chosen).json
        ? <label>{inputPrompt(chosen).label}<textarea className="mono" rows={3} value={input} onChange={(event) => setInput(event.target.value)} /></label>
        : <label>{inputPrompt(chosen).label}<input className="mono" value={input} onChange={(event) => setInput(event.target.value)} /></label>}
      <button className="primary-button" disabled={!ready} onClick={show}>Show it</button>
      <span className="present-hint centered">Esc to leave</span>
    </div>
  </div>;
}
