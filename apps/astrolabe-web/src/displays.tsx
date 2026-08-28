/**
 * The self-hosted display coordinator window.
 *
 * Facts are the daemon's display projection. Local state is draft form input
 * only; approvals, assignments, and revocations all cross as actions and
 * return through the ordinary authoritative refresh.
 */
import { useState } from "react";

import {
  actionKey,
  type ClientAction,
  type ClientView,
  type Display,
  type DisplayAssignment,
  type DisplayAssignmentRequest,
  type DisplayPairing,
  type DisplayReceiver,
  type DisplayRendezvous,
  type DisplayStaleAction,
  type DisplaySurface,
  type DisplaySyncMode,
  type DisplayTheme,
  type Orbit,
} from "./client";
import { AppDialog, Badge, DialogFooter, Empty, Fact, Notice, SectionTitle, shortId, words } from "./kit";

type Dispatch = (action: ClientAction) => Promise<void>;

/** The pinned receiver bootstrap a TV's setup screen accepts. */
export function receiverBootstrap(display: Pick<Display, "origin" | "certificateSha256" | "certificatePem">): string {
  return JSON.stringify({
    protocol_major: 1,
    trust: {
      kind: "pinned_certificate",
      origin: display.origin,
      sha256: display.certificateSha256,
    },
    certificate_pem: display.certificatePem,
    rendezvous: null,
  });
}

export function platformName(value: string): string {
  switch (value) {
    case "android_tv": return "Android TV";
    case "fire_tv": return "Fire TV";
    case "apple_tv": return "Apple TV";
    case "roku": return "Roku";
    case "webos": return "webOS";
    default: return words(value);
  }
}

/** The latest unrevoked assignment pinned on a receiver, if any. */
export function assignmentFor(display: Display, device: string): DisplayAssignment | undefined {
  for (let index = display.assignments.length - 1; index >= 0; index -= 1) {
    const assignment = display.assignments[index];
    if (assignment.device === device && assignment.revokedAtUnixMs === null) return assignment;
  }
  return undefined;
}

/** The daemon-validated bounds an assignment draft must meet before it may cross. */
export function assignmentDraftValid(draft: { input: string; staleSeconds: string; syncGroup: string; staticDelay: string }): boolean {
  const group = draft.syncGroup.trim();
  const delay = parseIntStrict(draft.staticDelay.trim());
  return draft.input.trim() !== ""
    && (parseIntStrict(draft.staleSeconds.trim()) ?? 0) >= 31
    && (group === "" || /^[a-z0-9_-]{1,64}$/.test(group))
    && delay !== null
    && delay >= -60_000
    && delay <= 60_000;
}

function parseIntStrict(value: string): number | null {
  return /^-?\d+$/.test(value) ? Number.parseInt(value, 10) : null;
}

const surfaceKey = (surface: DisplaySurface) => `${surface.world} ${surface.surface}`;

/** Everything an assignment form holds, as typed. One draft serves both the Assign dialog and a code's promise. */
export interface AssignmentDraft {
  orbit: string;
  chosenKey: string;
  input: string;
  theme: DisplayTheme;
  staleSeconds: string;
  onStale: DisplayStaleAction;
  syncGroup: string;
  syncMode: DisplaySyncMode;
  staticDelay: string;
}

export function newAssignmentDraft(surfaces: DisplaySurface[], orbits: Orbit[]): AssignmentDraft {
  return {
    orbit: orbits[0]?.space ?? "",
    chosenKey: surfaces[0] === undefined ? "" : surfaceKey(surfaces[0]),
    input: "",
    theme: "dark",
    staleSeconds: "120",
    onStale: "keepWithNativeBanner",
    syncGroup: "",
    syncMode: "stayInSync",
    staticDelay: "0",
  };
}

/**
 * The draft as the daemon takes it, or null while it could not cross. A
 * Signage program id is typed bare and wrapped here; every other surface's
 * input goes verbatim to the package's own canonicalizer.
 */
/**
 * Why the draft's input cannot cross yet, or null when it can. A signage
 * program id is any non-empty text; every other surface's input is JSON,
 * and the daemon's parser is the one that would otherwise say so — from
 * the other window, after the fact.
 */
export function inputProblem(draft: Pick<AssignmentDraft, "chosenKey" | "input">, surfaces: DisplaySurface[]): string | null {
  const chosen = surfaces.find((surface) => surfaceKey(surface) === draft.chosenKey);
  if (chosen === undefined) return "Choose a display surface.";
  const value = draft.input.trim();
  if (value === "") return isSignageSurface(chosen) ? "Enter the program's body id." : "Enter the package input as JSON.";
  if (isSignageSurface(chosen)) return null;
  try {
    JSON.parse(value);
    return null;
  } catch {
    return "The package input must be JSON — for example {\"project\":\"ENG\"}.";
  }
}

export function assignmentPayload(draft: AssignmentDraft, surfaces: DisplaySurface[]): DisplayAssignmentRequest | null {
  const chosen = surfaces.find((surface) => surfaceKey(surface) === draft.chosenKey);
  if (chosen === undefined || !assignmentDraftValid(draft) || inputProblem(draft, surfaces) !== null) return null;
  const value = draft.input.trim();
  return {
    orbit: draft.orbit,
    world: chosen.world,
    surface: chosen.surface,
    inputJson: isSignageSurface(chosen) ? JSON.stringify({ program: value }) : value,
    theme: draft.theme,
    staleAfterMs: Number.parseInt(draft.staleSeconds.trim(), 10) * 1000,
    onStale: draft.onStale,
    syncGroup: draft.syncGroup.trim() === "" ? null : draft.syncGroup.trim(),
    syncMode: draft.syncMode,
    staticDelayMs: Number.parseInt(draft.staticDelay.trim(), 10),
    expiresAtUnixMs: null,
  };
}

/** What a person enters on the television: the site, then the code — or the code alone where no site is published. */
export function codeEntry(minted: Pick<DisplayRendezvous, "site" | "code">): string {
  return minted.site === null ? minted.code : `${minted.site}-${minted.code}`;
}

/** Whole minutes a code has left, never below zero. */
export function minutesLeft(expiresAtUnixMs: number, now = Date.now()): number {
  return Math.max(0, Math.ceil((expiresAtUnixMs - now) / 60_000));
}

/**
 * The signage surface takes a single program body id, not free JSON — the
 * dialog offers the one field and spells the wrapping itself.
 */
export function isSignageSurface(surface: Pick<DisplaySurface, "world" | "surface">): boolean {
  return surface.world === "com.lait.signage" && surface.surface === "signage.program";
}

const themeNames: Record<DisplayTheme, string> = { light: "Light", dark: "Dark", highContrast: "High contrast" };
const staleActionNames: Record<DisplayStaleAction, string> = { keepWithNativeBanner: "Keep with native banner", blank: "Blank" };
const syncModeNames: Record<DisplaySyncMode, string> = { stayInSync: "Stay in sync", positional: "Positional" };

type DisplaysDialog =
  | { kind: "add" }
  | { kind: "approve"; pairing: DisplayPairing }
  | { kind: "assign"; receiver: DisplayReceiver }
  | { kind: "unassign"; assignment: DisplayAssignment }
  | { kind: "revoke"; receiver: DisplayReceiver }
  | { kind: "passphrase" };

export function DisplaysSurface({ view, dispatch, onBack, ownedWindow = false }: {
  view: ClientView; dispatch: Dispatch; onBack(): void; ownedWindow?: boolean;
}) {
  const [dialog, setDialog] = useState<DisplaysDialog | null>(null);
  const display = view.display;
  // A refusal of something asked from this window is answered in this
  // window. The main window's bar keeps the whole list; here only the
  // latest that concerns displays, which is the one the person is waiting on.
  const failure = view.failures.find((candidate) => /display/i.test(candidate.what) || /display/i.test(candidate.error));
  return <section className="secondary-surface" aria-label="Displays">
    <header className="secondary-header">
      <button className="back-button" onClick={onBack}>{ownedWindow ? "Close window" : "← Library"}</button>
      <div><h1>Displays</h1><p>Enroll receivers on this network and pin each one to an exact World surface in an Orbit.</p></div>
      <div className="header-actions">
        <button className="quiet-button" disabled={view.inFlight.includes(actionKey.refresh)}
          onClick={() => void dispatch({ type: "refresh" })}>Refresh</button>
        <button className="primary-button" disabled={display === null}
          onClick={() => setDialog({ kind: "add" })}>Add a display…</button>
      </div>
    </header>
    {display === null
      ? <div className="secondary-scroll"><div className="skeleton" style={{ height: 112, marginBottom: 22 }} />
          <div className="skeleton" style={{ height: 152, marginBottom: 9 }} /><div className="skeleton" style={{ height: 152 }} /></div>
      : <div className="secondary-scroll displays-surface">
        {failure !== undefined && <Notice tone="danger">{failure.what}: {failure.error}</Notice>}
        <Coordinator display={display} openDialog={setDialog} />
        {display.pendingRendezvous.length > 0 && <section className="section-block">
          <SectionTitle label="CODES WAITING" count={display.pendingRendezvous.length} />
          {display.pendingRendezvous.map((minted) => <RendezvousCard key={minted.rendezvous} minted={minted}
            view={view} dispatch={dispatch} />)}
        </section>}
        <section className="section-block">
          <SectionTitle label="PAIRING REQUESTS" count={display.pendingPairings.length} />
          {display.pendingPairings.length === 0
            ? <Empty said="No receiver is waiting for approval."
                next="Add a display for a code the TV enters, or open Astrolabe on a TV and compare words here." />
            : display.pendingPairings.map((pairing) => <PairingCard key={pairing.pairing} pairing={pairing} view={view}
                dispatch={dispatch} onApprove={() => setDialog({ kind: "approve", pairing })} />)}
        </section>
        <section className="section-block">
          <SectionTitle label="RECEIVERS" count={display.devices.length} />
          {display.devices.length === 0
            ? <Empty said="No receiver is enrolled." next="Pairing is confirmed on both the TV and here." />
            : display.devices.map((receiver) => <ReceiverCard key={receiver.device} receiver={receiver}
                assignment={assignmentFor(display, receiver.device)} display={display} view={view} openDialog={setDialog} />)}
        </section>
      </div>}
    {dialog !== null && display !== null && <DisplaysDialogs dialog={dialog} display={display} orbits={view.orbits}
      dispatch={dispatch} onDismiss={() => setDialog(null)} />}
  </section>;
}

function Coordinator({ display, openDialog }: { display: Display; openDialog(dialog: DisplaysDialog): void }) {
  return <section className="coordinator-card">
    <div className="coordinator-title">
      <strong>{display.label}</strong>
      <span className="button-row">
        <button className="quiet-button" title="Copy the pinned receiver bootstrap JSON."
          onClick={() => void navigator.clipboard.writeText(receiverBootstrap(display))}>Copy setup</button>
        <Badge label="SELF-HOSTED" />
      </span>
    </div>
    <Fact label="LAN ORIGIN" value={display.origin} />
    <Fact label="CERTIFICATE SHA-256" value={display.certificateSha256} />
    <IdentifierCustodyFacts custody={display.identifierCustody}
      onAddPassphrase={() => openDialog({ kind: "passphrase" })} />
  </section>;
}

const slotNames: Record<string, string> = {
  "recovery-key": "this identity",
  "passphrase": "a passphrase",
  "windows-dpapi": "this Windows profile",
};

/**
 * On the coordinator card, not behind settings: the moment an operator wants
 * this fact is after the machine is gone, and a warning only reachable from
 * the lost machine is not a warning.
 */
function IdentifierCustodyFacts({ custody, onAddPassphrase }: {
  custody: Display["identifierCustody"]; onAddPassphrase(): void;
}) {
  if (custody === null) {
    return <Fact label="IDENTIFIER KEY UNLOCKS" value="not reported by this coordinator" />;
  }
  const paths = custody.slots.length === 0 ? "none" : custody.slots.map((slot) => slotNames[slot] ?? slot).join(", ");
  // Offered once: the store refuses a second passphrase, and a control that
  // would be refused is one this surface should not draw.
  const hasPassphrase = custody.slots.includes("passphrase");
  return <div className="identifier-custody">
    <div className="coordinator-title">
      <Fact label="IDENTIFIER KEY UNLOCKS" value={paths} />
      {!hasPassphrase && <button className="quiet-button"
        title="A way in that survives losing this machine and this identity."
        onClick={onAddPassphrase}>Add a passphrase</button>}
    </div>
    <p className="custody-note" data-warning={!custody.portable || undefined}>
      {custody.portable
        ? "Losing every unlock path invalidates the item and asset identifiers already delivered to paired screens. They would each need pairing again."
        : "Every unlock path is bound to this machine. Losing this profile invalidates the item and asset identifiers already delivered to paired screens, and they would each need pairing again. Add a passphrase or a second device."}
    </p>
  </div>;
}

/**
 * A code waiting to be entered. Shown as the television wants it — site,
 * then code — with what entering it will do, and how long that holds.
 */
function RendezvousCard({ minted, view, dispatch }: { minted: DisplayRendezvous; view: ClientView; dispatch: Dispatch }) {
  const busy = view.inFlight.includes(actionKey.displayRendezvousRevoke(minted.rendezvous));
  const left = minutesLeft(minted.expiresAtUnixMs);
  const entry = codeEntry(minted);
  return <article className="receiver-card rendezvous-card">
    <div className="receiver-title">
      <div>
        <strong>{minted.label}</strong>
        <small>{minted.assignment === null
          ? "Enrols, then waits for an assignment"
          : `Shows ${minted.assignment.world} · ${minted.assignment.surface} as soon as it connects`}</small>
      </div>
      <Badge label={left === 0 ? "EXPIRED" : `${left} MIN LEFT`} />
    </div>
    <p className="rendezvous-code" aria-label="Code to enter on the television">{entry}</p>
    <p className="health-line">{minted.site === null
      ? "This coordinator publishes no site, so the television must already reach it; enter the code where it asks."
      : "On the television: open Astrolabe, enter this where it asks for the code, press OK."}</p>
    <div className="button-row end">
      <button className="quiet-button" onClick={() => void navigator.clipboard.writeText(entry)}>Copy</button>
      <button className="quiet-button" disabled={busy}
        onClick={() => void dispatch({ type: "displayRendezvousRevoke", rendezvous: minted.rendezvous })}>Withdraw</button>
    </div>
  </article>;
}

function PairingCard({ pairing, view, dispatch, onApprove }: {
  pairing: DisplayPairing; view: ClientView; dispatch: Dispatch; onApprove(): void;
}) {
  const busy = view.inFlight.includes(actionKey.displayPairingApprove(pairing.pairing))
    || view.inFlight.includes(actionKey.displayPairingReject(pairing.pairing));
  return <article className="receiver-card">
    <div className="receiver-title">
      <strong>{platformName(pairing.platform)} · {pairing.build}</strong>
      <Badge label="VERIFY ON TV" />
    </div>
    <p className="phrase">{pairing.confirmationPhrase.join("  ")}</p>
    <Fact label="CERTIFICATE SHA-256" value={pairing.certificateSha256} />
    <div className="button-row end">
      <button className="quiet-button" disabled={busy}
        onClick={() => void dispatch({ type: "displayPairingReject", pairing: pairing.pairing })}>Reject</button>
      <button className="primary-button" disabled={busy} onClick={onApprove}>Approve…</button>
    </div>
  </article>;
}

function ReceiverCard({ receiver, assignment, display, view, openDialog }: {
  receiver: DisplayReceiver; assignment: DisplayAssignment | undefined; display: Display; view: ClientView;
  openDialog(dialog: DisplaysDialog): void;
}) {
  const revoked = receiver.revokedAtUnixMs !== null;
  const health = receiver.health;
  const assigning = view.inFlight.includes(actionKey.displayAssignmentPut(receiver.device));
  const revokingDevice = view.inFlight.includes(actionKey.displayDeviceRevoke(receiver.device));
  const revokingAssignment = assignment !== undefined
    && view.inFlight.includes(actionKey.displayAssignmentRevoke(assignment.assignment));
  const cannotAssign = view.orbits.length === 0
    ? "This identity has no Orbit to assign."
    : display.surfaces.length === 0
      ? "This build declares no display surfaces."
      : null;
  return <article className="receiver-card">
    <div className="receiver-title">
      <div>
        <strong>{receiver.label}</strong>
        <small>{platformName(receiver.platform)} · {receiver.build}</small>
      </div>
      <Badge label={revoked ? "REVOKED" : health === null ? "NOT YET REPORTED" : health.connection.toUpperCase()} />
    </div>
    {assignment === undefined
      ? <p className="health-line">Unassigned</p>
      : <div className="assignment-facts">
        <strong>{assignment.world} · {assignment.surface}</strong>
        <code>Orbit {shortId(assignment.orbit)} · program {shortId(assignment.program)}</code>
        {assignment.syncGroup !== null && <small>
          Sync {assignment.syncGroup} · {assignment.syncMode === null ? "" : syncModeNames[assignment.syncMode]} ·{" "}
          {assignment.staticDelayMs >= 0 ? "+" : ""}{assignment.staticDelayMs} ms
        </small>}
      </div>}
    {health !== null && <>
      <p className="health-line">{words(health.playback)} · item {shortId(health.currentItem)} · {health.elapsedMs} ms</p>
      {assignment?.syncGroup != null && <p className="health-line">
        Residual {health.driftResidualMs} ms · {health.correctionEvents} corrections
      </p>}
      {health.lastError !== "none" && <p className="health-line error-line">Receiver reports {words(health.lastError)}</p>}
    </>}
    <div className="button-row wrap">
      {!revoked && <button className="quiet-button" disabled={assigning || cannotAssign !== null}
        title={cannotAssign ?? undefined}
        onClick={() => openDialog({ kind: "assign", receiver })}>{assignment === undefined ? "Assign…" : "Replace…"}</button>}
      {assignment !== undefined && <button className="quiet-button" disabled={revokingAssignment}
        onClick={() => openDialog({ kind: "unassign", assignment })}>Unassign</button>}
      {!revoked && <button className="danger-button" disabled={revokingDevice}
        onClick={() => openDialog({ kind: "revoke", receiver })}>Revoke receiver…</button>}
    </div>
  </article>;
}

function DisplaysDialogs({ dialog, display, orbits, dispatch, onDismiss }: {
  dialog: DisplaysDialog; display: Display; orbits: Orbit[]; dispatch: Dispatch; onDismiss(): void;
}) {
  switch (dialog.kind) {
    case "add": return <AddDisplayDialog display={display} orbits={orbits} dispatch={dispatch} onDismiss={onDismiss} />;
    case "passphrase": return <PassphraseDialog dispatch={dispatch} onDismiss={onDismiss} />;
    case "approve": return <ApproveDialog pairing={dialog.pairing} dispatch={dispatch} onDismiss={onDismiss} />;
    case "assign": return <AssignDialog receiver={dialog.receiver} surfaces={display.surfaces} orbits={orbits}
      dispatch={dispatch} onDismiss={onDismiss} />;
    case "unassign": return <AppDialog title="Unassign this display?"
      description="The receiver will get an unassigned program on its next poll." onDismiss={onDismiss}>
      <DialogFooter>
        <button className="quiet-button" onClick={onDismiss}>Cancel</button>
        <button className="primary-button" onClick={() => {
          void dispatch({ type: "displayAssignmentRevoke", assignment: dialog.assignment.assignment });
          onDismiss();
        }}>Unassign</button>
      </DialogFooter>
    </AppDialog>;
    case "revoke": return <AppDialog title={`Revoke ${dialog.receiver.label}?`}
      description="Its proof key will stop working immediately. Reconnecting this receiver requires a new pairing ceremony."
      onDismiss={onDismiss}>
      <DialogFooter>
        <button className="quiet-button" onClick={onDismiss}>Cancel</button>
        <button className="danger-button" onClick={() => {
          void dispatch({ type: "displayDeviceRevoke", device: dialog.receiver.device });
          onDismiss();
        }}>Revoke receiver</button>
      </DialogFooter>
    </AppDialog>;
  }
}

/// A convenience floor so the control can refuse before a round trip; the
/// daemon refuses shorter ones too, and its check is the real one.
const minPassphrase = 12;

function PassphraseDialog({ dispatch, onDismiss }: { dispatch: Dispatch; onDismiss(): void }) {
  const [entered, setEntered] = useState("");
  const [again, setAgain] = useState("");
  const long = [...entered].length >= minPassphrase;
  const matches = entered === again;
  return <AppDialog title="Add a passphrase"
    description="A second way into the identifier key, independent of this machine and this identity. It is not stored — it wraps the key and is forgotten, so losing it costs this path and nothing else."
    onDismiss={onDismiss}>
    <label>Passphrase<input type="password" value={entered} onChange={(event) => setEntered(event.target.value)} /></label>
    {/* Typed twice because it cannot be recovered and cannot be shown back:
        a mistyped passphrase would look like a working slot until the day it
        was the only one left. */}
    <label>Again<input type="password" value={again} onChange={(event) => setAgain(event.target.value)} /></label>
    {entered !== "" && !long
      ? <p className="custody-note">At least {minPassphrase} characters.</p>
      : again !== "" && !matches
        ? <p className="custody-note">These do not match.</p>
        : null}
    <DialogFooter>
      <button className="quiet-button" onClick={onDismiss}>Cancel</button>
      <button className="primary-button" disabled={!long || !matches} onClick={() => {
        void dispatch({ type: "displayIdentifierAdmitPassphrase", passphrase: entered });
        onDismiss();
      }}>Add it</button>
    </DialogFooter>
  </AppDialog>;
}

function ApproveDialog({ pairing, dispatch, onDismiss }: { pairing: DisplayPairing; dispatch: Dispatch; onDismiss(): void }) {
  const [label, setLabel] = useState(platformName(pairing.platform));
  return <AppDialog title="Approve this display?"
    description="Continue only if the six words and certificate fingerprint exactly match the receiver screen."
    onDismiss={onDismiss}>
    <p className="phrase">{pairing.confirmationPhrase.join("  ")}</p>
    <label>Display name<input value={label} onChange={(event) => setLabel(event.target.value)} /></label>
    <DialogFooter>
      <button className="quiet-button" onClick={onDismiss}>Cancel</button>
      <button className="primary-button" disabled={label.trim() === ""} onClick={() => {
        void dispatch({ type: "displayPairingApprove", pairing: pairing.pairing, label: label.trim() });
        onDismiss();
      }}>Approve</button>
    </DialogFooter>
  </AppDialog>;
}

/** The assignment form: what to show, where from, and how it goes stale. */
function AssignmentDraftFields({ draft, setDraft, surfaces, orbits }: {
  draft: AssignmentDraft; setDraft(next: AssignmentDraft): void; surfaces: DisplaySurface[]; orbits: Orbit[];
}) {
  const chosen = surfaces.find((surface) => surfaceKey(surface) === draft.chosenKey);
  const signage = chosen !== undefined && isSignageSurface(chosen);
  const problem = inputProblem(draft, surfaces);
  const set = <K extends keyof AssignmentDraft>(key: K, value: AssignmentDraft[K]) => setDraft({ ...draft, [key]: value });
  return <>
    <label>ORBIT<select value={draft.orbit} onChange={(event) => set("orbit", event.target.value)}>
      {orbits.map((row) => <option key={row.space} value={row.space}>{row.name}</option>)}
    </select></label>
    <label>DISPLAY SURFACE<select value={draft.chosenKey} onChange={(event) => setDraft({ ...draft, chosenKey: event.target.value, input: "" })}>
      {surfaces.map((surface) => <option key={surfaceKey(surface)} value={surfaceKey(surface)}>
        {surface.title} · {surface.world}
      </option>)}
    </select></label>
    {signage
      ? <label>Signage program body ID<input className="mono" value={draft.input} onChange={(event) => set("input", event.target.value)} /></label>
      : <label>Package input JSON<textarea className="mono" rows={4} value={draft.input} onChange={(event) => set("input", event.target.value)} /></label>}
    {draft.input.trim() !== "" && problem !== null && <p className="custody-note" data-warning>{problem}</p>}
    <div className="dialog-grid">
      <label>THEME<select value={draft.theme} onChange={(event) => set("theme", event.target.value as DisplayTheme)}>
        {(Object.keys(themeNames) as DisplayTheme[]).map((option) => <option key={option} value={option}>{themeNames[option]}</option>)}
      </select></label>
      <label>Stale after (seconds)<input value={draft.staleSeconds} onChange={(event) => set("staleSeconds", event.target.value)} /></label>
    </div>
    <label>WHEN STALE<select value={draft.onStale} onChange={(event) => set("onStale", event.target.value as DisplayStaleAction)}>
      {(Object.keys(staleActionNames) as DisplayStaleAction[]).map((option) =>
        <option key={option} value={option}>{staleActionNames[option]}</option>)}
    </select></label>
    <label>Sync group (optional)<input className="mono" value={draft.syncGroup} onChange={(event) => set("syncGroup", event.target.value)} /></label>
    <div className="dialog-grid">
      <label>SYNC MODE<select value={draft.syncMode} onChange={(event) => set("syncMode", event.target.value as DisplaySyncMode)}>
        {(Object.keys(syncModeNames) as DisplaySyncMode[]).map((option) =>
          <option key={option} value={option}>{syncModeNames[option]}</option>)}
      </select></label>
      <label>Static delay (ms, + advances)<input value={draft.staticDelay} onChange={(event) => set("staticDelay", event.target.value)} /></label>
    </div>
  </>;
}

function AssignDialog({ receiver, surfaces, orbits, dispatch, onDismiss }: {
  receiver: DisplayReceiver; surfaces: DisplaySurface[]; orbits: Orbit[]; dispatch: Dispatch; onDismiss(): void;
}) {
  const [draft, setDraft] = useState(() => newAssignmentDraft(surfaces, orbits));
  const payload = assignmentPayload(draft, surfaces);
  const assign = () => {
    if (payload === null) return;
    void dispatch({ type: "displayAssignmentPut", device: receiver.device, ...payload });
    onDismiss();
  };
  return <AppDialog title={`Assign ${receiver.label}`}
    description="The daemon validates the package input, queries the exact Orbit, and pins the resulting receiver program."
    onDismiss={onDismiss}>
    <AssignmentDraftFields draft={draft} setDraft={setDraft} surfaces={surfaces} orbits={orbits} />
    <DialogFooter>
      <button className="quiet-button" onClick={onDismiss}>Cancel</button>
      <button className="primary-button" disabled={payload === null} onClick={assign}>Assign</button>
    </DialogFooter>
  </AppDialog>;
}

/**
 * A code for a television to enter: the whole of enrolment, and — with a
 * promise — the whole of "connect this screen to the lobby loop", as one act
 * at the desk. Minting is what the button does; the code itself arrives on
 * the surface with the next authoritative refresh, under "codes waiting".
 */
function AddDisplayDialog({ display, orbits, dispatch, onDismiss }: {
  display: Display; orbits: Orbit[]; dispatch: Dispatch; onDismiss(): void;
}) {
  const canPromise = display.surfaces.length > 0 && orbits.length > 0;
  const [label, setLabel] = useState("");
  const [promise, setPromise] = useState(canPromise);
  const [draft, setDraft] = useState(() => newAssignmentDraft(display.surfaces, orbits));
  const payload = promise ? assignmentPayload(draft, display.surfaces) : null;
  const ready = label.trim() !== "" && (!promise || payload !== null);
  const mint = () => {
    if (!ready) return;
    void dispatch({ type: "displayRendezvousMint", label: label.trim(), assignment: payload });
    onDismiss();
  };
  return <AppDialog title="Add a display"
    description="A code the television enters. It works once and lasts fifteen minutes, and it stands in for comparing words: whoever enters it enrols that screen as this display."
    onDismiss={onDismiss}>
    <label>Display name<input value={label} placeholder="Lobby" autoFocus onChange={(event) => setLabel(event.target.value)} /></label>
    {canPromise
      ? <label className="check-row"><input type="checkbox" checked={promise} onChange={(event) => setPromise(event.target.checked)} />
          Show something as soon as it connects</label>
      : <p className="custody-note">{display.surfaces.length === 0
          ? "No selected World declares a display surface, so the display will enrol and wait for an assignment."
          : "This identity has no Orbit to draw from, so the display will enrol and wait for an assignment."}</p>}
    {promise && canPromise && <AssignmentDraftFields draft={draft} setDraft={setDraft} surfaces={display.surfaces} orbits={orbits} />}
    <DialogFooter>
      <button className="quiet-button" onClick={onDismiss}>Cancel</button>
      <button className="primary-button" disabled={!ready} onClick={mint}>Get a code</button>
    </DialogFooter>
  </AppDialog>;
}
