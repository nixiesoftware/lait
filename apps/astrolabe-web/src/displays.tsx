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
  type DisplayPairing,
  type DisplayReceiver,
  type DisplayStaleAction,
  type DisplaySurface,
  type DisplaySyncMode,
  type DisplayTheme,
  type Orbit,
} from "./client";
import { AppDialog, Badge, DialogFooter, Empty, Fact, SectionTitle, shortId, words } from "./kit";

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
  return <section className="secondary-surface" aria-label="Displays">
    <header className="secondary-header">
      <button className="back-button" onClick={onBack}>{ownedWindow ? "Close window" : "← Library"}</button>
      <div><h1>Displays</h1><p>Enroll receivers on this network and pin each one to an exact World surface in an Orbit.</p></div>
      <div className="header-actions">
        <button className="quiet-button" disabled={view.inFlight.includes(actionKey.refresh)}
          onClick={() => void dispatch({ type: "refresh" })}>Refresh</button>
      </div>
    </header>
    {display === null
      ? <div className="secondary-scroll"><div className="skeleton" style={{ height: 112, marginBottom: 22 }} />
          <div className="skeleton" style={{ height: 152, marginBottom: 9 }} /><div className="skeleton" style={{ height: 152 }} /></div>
      : <div className="secondary-scroll displays-surface">
        <Coordinator display={display} openDialog={setDialog} />
        <section className="section-block">
          <SectionTitle label="PAIRING REQUESTS" count={display.pendingPairings.length} />
          {display.pendingPairings.length === 0
            ? <Empty said="No receiver is waiting for approval." next="Open Astrolabe setup on a TV to begin pairing." />
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

const surfaceKey = (surface: DisplaySurface) => `${surface.world} ${surface.surface}`;

function AssignDialog({ receiver, surfaces, orbits, dispatch, onDismiss }: {
  receiver: DisplayReceiver; surfaces: DisplaySurface[]; orbits: Orbit[]; dispatch: Dispatch; onDismiss(): void;
}) {
  const [orbit, setOrbit] = useState(orbits[0]?.space ?? "");
  const [chosenKey, setChosenKey] = useState(surfaces[0] === undefined ? "" : surfaceKey(surfaces[0]));
  const [input, setInput] = useState("");
  const [theme, setTheme] = useState<DisplayTheme>("dark");
  const [staleSeconds, setStaleSeconds] = useState("120");
  const [onStale, setOnStale] = useState<DisplayStaleAction>("keepWithNativeBanner");
  const [syncGroup, setSyncGroup] = useState("");
  const [syncMode, setSyncMode] = useState<DisplaySyncMode>("stayInSync");
  const [staticDelay, setStaticDelay] = useState("0");
  const chosen = surfaces.find((surface) => surfaceKey(surface) === chosenKey);
  const signage = chosen !== undefined && isSignageSurface(chosen);
  const valid = chosen !== undefined && assignmentDraftValid({ input, staleSeconds, syncGroup, staticDelay });
  const assign = () => {
    if (chosen === undefined || !valid) return;
    const value = input.trim();
    void dispatch({
      type: "displayAssignmentPut",
      device: receiver.device,
      orbit,
      world: chosen.world,
      surface: chosen.surface,
      inputJson: signage ? JSON.stringify({ program: value }) : value,
      theme,
      staleAfterMs: Number.parseInt(staleSeconds.trim(), 10) * 1000,
      onStale,
      syncGroup: syncGroup.trim() === "" ? null : syncGroup.trim(),
      syncMode,
      staticDelayMs: Number.parseInt(staticDelay.trim(), 10),
      expiresAtUnixMs: null,
    });
    onDismiss();
  };
  return <AppDialog title={`Assign ${receiver.label}`}
    description="The daemon validates the package input, queries the exact Orbit, and pins the resulting receiver program."
    onDismiss={onDismiss}>
    <label>ORBIT<select value={orbit} onChange={(event) => setOrbit(event.target.value)}>
      {orbits.map((row) => <option key={row.space} value={row.space}>{row.name}</option>)}
    </select></label>
    <label>DISPLAY SURFACE<select value={chosenKey} onChange={(event) => {
      setChosenKey(event.target.value);
      setInput("");
    }}>
      {surfaces.map((surface) => <option key={surfaceKey(surface)} value={surfaceKey(surface)}>
        {surface.title} · {surface.world}
      </option>)}
    </select></label>
    {signage
      ? <label>Signage program body ID<input className="mono" value={input} onChange={(event) => setInput(event.target.value)} /></label>
      : <label>Package input JSON<textarea className="mono" rows={4} value={input} onChange={(event) => setInput(event.target.value)} /></label>}
    <div className="dialog-grid">
      <label>THEME<select value={theme} onChange={(event) => setTheme(event.target.value as DisplayTheme)}>
        {(Object.keys(themeNames) as DisplayTheme[]).map((option) => <option key={option} value={option}>{themeNames[option]}</option>)}
      </select></label>
      <label>Stale after (seconds)<input value={staleSeconds} onChange={(event) => setStaleSeconds(event.target.value)} /></label>
    </div>
    <label>WHEN STALE<select value={onStale} onChange={(event) => setOnStale(event.target.value as DisplayStaleAction)}>
      {(Object.keys(staleActionNames) as DisplayStaleAction[]).map((option) =>
        <option key={option} value={option}>{staleActionNames[option]}</option>)}
    </select></label>
    <label>Sync group (optional)<input className="mono" value={syncGroup} onChange={(event) => setSyncGroup(event.target.value)} /></label>
    <div className="dialog-grid">
      <label>SYNC MODE<select value={syncMode} onChange={(event) => setSyncMode(event.target.value as DisplaySyncMode)}>
        {(Object.keys(syncModeNames) as DisplaySyncMode[]).map((option) =>
          <option key={option} value={option}>{syncModeNames[option]}</option>)}
      </select></label>
      <label>Static delay (ms, + advances)<input value={staticDelay} onChange={(event) => setStaticDelay(event.target.value)} /></label>
    </div>
    <DialogFooter>
      <button className="quiet-button" onClick={onDismiss}>Cancel</button>
      <button className="primary-button" disabled={!valid} onClick={assign}>Assign</button>
    </DialogFooter>
  </AppDialog>;
}
