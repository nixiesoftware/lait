/**
 * The self-hosted display coordinator window.
 *
 * Facts are the daemon's display projection. Local state is draft form input
 * only; approvals, assignments, and revocations all cross as actions and
 * return through the ordinary authoritative refresh.
 *
 * The first surface drawn with Fluent: every control is the system's, and
 * what is Astrolabe's — the code a television takes, the receiver's health
 * line — is spelled with the system's tokens rather than a stylesheet.
 */
import {
  Body1,
  Button,
  Card,
  CardFooter,
  CardHeader,
  Caption1,
  Checkbox,
  Field,
  Input,
  Select,
  Text,
  Textarea,
  Title3,
  makeStyles,
  mergeClasses,
  shorthands,
  tokens,
} from "@fluentui/react-components";
import {
  Add20Regular,
  ArrowLeft20Regular,
  ArrowSync20Regular,
  Copy20Regular,
  Dismiss20Regular,
} from "@fluentui/react-icons";
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

/**
 * The draft as the daemon takes it, or null while it could not cross. A
 * Signage program id is typed bare and wrapped here; every other surface's
 * input goes verbatim to the package's own canonicalizer.
 */
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

const useStyles = makeStyles({
  surface: {
    height: "100%",
    minHeight: 0,
    display: "grid",
    gridTemplateRows: "auto minmax(0, 1fr)",
    backgroundColor: tokens.colorNeutralBackground2,
    color: tokens.colorNeutralForeground1,
  },
  header: {
    display: "grid",
    gridTemplateColumns: "auto minmax(0, 1fr) auto",
    alignItems: "center",
    columnGap: tokens.spacingHorizontalL,
    padding: `${tokens.spacingVerticalM} ${tokens.spacingHorizontalXL}`,
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
    backgroundColor: tokens.colorNeutralBackground1,
  },
  headerCopy: { display: "grid", gap: tokens.spacingVerticalXXS, minWidth: 0 },
  headerActions: { display: "flex", gap: tokens.spacingHorizontalS },
  scroll: {
    overflowY: "auto",
    padding: `${tokens.spacingVerticalL} ${tokens.spacingHorizontalXL} ${tokens.spacingVerticalXXL}`,
  },
  column: { maxWidth: "920px", margin: "0 auto", display: "grid", gap: tokens.spacingVerticalM },
  cards: { display: "grid", gap: tokens.spacingVerticalS },
  stack: { display: "grid", gap: tokens.spacingVerticalXS },
  row: { display: "flex", alignItems: "center", gap: tokens.spacingHorizontalS, flexWrap: "nowrap", flexShrink: 0, whiteSpace: "nowrap" },
  end: { justifyContent: "flex-end" },
  between: { display: "flex", alignItems: "center", justifyContent: "space-between", gap: tokens.spacingHorizontalM },
  muted: { color: tokens.colorNeutralForeground3 },
  warn: { color: tokens.colorPaletteMarigoldForeground2 },
  danger: { color: tokens.colorPaletteRedForeground1 },
  phrase: {
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase400,
    fontWeight: tokens.fontWeightSemibold,
    color: tokens.colorBrandForeground1,
    letterSpacing: "0.04em",
  },
  code: {
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeHero700,
    fontWeight: tokens.fontWeightSemibold,
    letterSpacing: "0.1em",
    userSelect: "all",
    margin: 0,
  },
  codeCard: { ...shorthands.borderColor(tokens.colorBrandStroke1) },
  facts: { display: "grid", gap: tokens.spacingVerticalXXS },
  grid2: { display: "grid", gridTemplateColumns: "repeat(2, minmax(0, 1fr))", gap: tokens.spacingHorizontalM },
});

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
  const styles = useStyles();
  const [dialog, setDialog] = useState<DisplaysDialog | null>(null);
  const display = view.display;
  // A refusal of something asked from this window is answered in this
  // window. The main window's bar keeps the whole list; here only the
  // latest that concerns displays, which is the one the person is waiting on.
  const failure = view.failures.find((candidate) => /display/i.test(candidate.what) || /display/i.test(candidate.error));
  return <section className={styles.surface} aria-label="Displays">
    <header className={styles.header}>
      <Button appearance="subtle" icon={ownedWindow ? <Dismiss20Regular /> : <ArrowLeft20Regular />} onClick={onBack}>
        {ownedWindow ? "Close window" : "Library"}
      </Button>
      <div className={styles.headerCopy}>
        <Title3>Displays</Title3>
        <Caption1 className={styles.muted}>Enroll receivers on this network and pin each one to an exact World surface in an Orbit.</Caption1>
      </div>
      <div className={styles.headerActions}>
        <Button appearance="secondary" icon={<ArrowSync20Regular />} disabled={view.inFlight.includes(actionKey.refresh)}
          onClick={() => void dispatch({ type: "refresh" })}>Refresh</Button>
        <Button appearance="primary" icon={<Add20Regular />} disabled={display === null}
          onClick={() => setDialog({ kind: "add" })}>Add a display</Button>
      </div>
    </header>
    <div className={styles.scroll}>
      {display === null
        ? <div className={styles.column}><Empty said="Reading the display coordinator…" /></div>
        : <div className={styles.column}>
          {failure !== undefined && <Notice tone="danger">{failure.what}: {failure.error}</Notice>}
          <Coordinator display={display} openDialog={setDialog} />
          {display.pendingRendezvous.length > 0 && <section>
            <SectionTitle label="Codes waiting" count={display.pendingRendezvous.length} />
            <div className={styles.cards}>
              {display.pendingRendezvous.map((minted) => <RendezvousCard key={minted.rendezvous} minted={minted}
                view={view} dispatch={dispatch} />)}
            </div>
          </section>}
          <section>
            <SectionTitle label="Pairing requests" count={display.pendingPairings.length} />
            {display.pendingPairings.length === 0
              ? <Empty said="No receiver is waiting for approval."
                  next="Add a display for a code the TV enters, or open Astrolabe on a TV and compare words here." />
              : <div className={styles.cards}>
                {display.pendingPairings.map((pairing) => <PairingCard key={pairing.pairing} pairing={pairing} view={view}
                  dispatch={dispatch} onApprove={() => setDialog({ kind: "approve", pairing })} />)}
              </div>}
          </section>
          <section>
            <SectionTitle label="Receivers" count={display.devices.length} />
            {display.devices.length === 0
              ? <Empty said="No receiver is enrolled." next="Pairing is confirmed on both the TV and here." />
              : <div className={styles.cards}>
                {display.devices.map((receiver) => <ReceiverCard key={receiver.device} receiver={receiver}
                  assignment={assignmentFor(display, receiver.device)} display={display} view={view} openDialog={setDialog} />)}
              </div>}
          </section>
        </div>}
    </div>
    {dialog !== null && display !== null && <DisplaysDialogs dialog={dialog} display={display} orbits={view.orbits}
      dispatch={dispatch} onDismiss={() => setDialog(null)} />}
  </section>;
}

function Coordinator({ display, openDialog }: { display: Display; openDialog(dialog: DisplaysDialog): void }) {
  const styles = useStyles();
  return <Card>
    <CardHeader
      header={<Text weight="semibold" size={500}>{display.label}</Text>}
      description={<Caption1 className={styles.muted}>This coordinator</Caption1>}
      action={<div className={styles.row}>
        <Button appearance="secondary" size="small" icon={<Copy20Regular />} title="Copy the pinned receiver bootstrap JSON."
          onClick={() => void navigator.clipboard.writeText(receiverBootstrap(display))}>Copy setup</Button>
        <Badge label="Self-hosted" />
      </div>} />
    <div className={styles.facts}>
      <Fact label="LAN origin" value={display.origin} />
      <Fact label="Certificate SHA-256" value={display.certificateSha256} />
    </div>
    <IdentifierCustodyFacts custody={display.identifierCustody}
      onAddPassphrase={() => openDialog({ kind: "passphrase" })} />
  </Card>;
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
  const styles = useStyles();
  if (custody === null) {
    return <Fact label="Identifier key unlocks" value="not reported by this coordinator" />;
  }
  const paths = custody.slots.length === 0 ? "none" : custody.slots.map((slot) => slotNames[slot] ?? slot).join(", ");
  // Offered once: the store refuses a second passphrase, and a control that
  // would be refused is one this surface should not draw.
  const hasPassphrase = custody.slots.includes("passphrase");
  return <div className={styles.stack}>
    <div className={styles.between}>
      <Fact label="Identifier key unlocks" value={paths} />
      {!hasPassphrase && <Button appearance="secondary" size="small"
        title="A way in that survives losing this machine and this identity."
        onClick={onAddPassphrase}>Add a passphrase</Button>}
    </div>
    <Caption1 className={custody.portable ? styles.muted : styles.warn}>
      {custody.portable
        ? "Losing every unlock path invalidates the item and asset identifiers already delivered to paired screens. They would each need pairing again."
        : "Every unlock path is bound to this machine. Losing this profile invalidates the item and asset identifiers already delivered to paired screens, and they would each need pairing again. Add a passphrase or a second device."}
    </Caption1>
  </div>;
}

/**
 * A code waiting to be entered. Shown as the television wants it — site,
 * then code — with what entering it will do, and how long that holds.
 */
function RendezvousCard({ minted, view, dispatch }: { minted: DisplayRendezvous; view: ClientView; dispatch: Dispatch }) {
  const styles = useStyles();
  const busy = view.inFlight.includes(actionKey.displayRendezvousRevoke(minted.rendezvous));
  const left = minutesLeft(minted.expiresAtUnixMs);
  const entry = codeEntry(minted);
  return <Card className={styles.codeCard}>
    <CardHeader
      header={<Text weight="semibold">{minted.label}</Text>}
      description={<Caption1 className={styles.muted}>{minted.assignment === null
        ? "Enrols, then waits for an assignment"
        : `Shows ${minted.assignment.world} · ${minted.assignment.surface} as soon as it connects`}</Caption1>}
      action={<Badge label={left === 0 ? "Expired" : `${left} min left`} solid={left > 0} />} />
    <p className={styles.code} aria-label="Code to enter on the television">{entry}</p>
    <Caption1 className={styles.muted}>{minted.site === null
      ? "This coordinator publishes no site, so the television must already reach it; enter the code where it asks."
      : "On the television: open Astrolabe, enter this where it asks for the code, press OK."}</Caption1>
    <CardFooter>
      <Button appearance="secondary" icon={<Copy20Regular />} onClick={() => void navigator.clipboard.writeText(entry)}>Copy</Button>
      <Button appearance="subtle" disabled={busy}
        onClick={() => void dispatch({ type: "displayRendezvousRevoke", rendezvous: minted.rendezvous })}>Withdraw</Button>
    </CardFooter>
  </Card>;
}

function PairingCard({ pairing, view, dispatch, onApprove }: {
  pairing: DisplayPairing; view: ClientView; dispatch: Dispatch; onApprove(): void;
}) {
  const styles = useStyles();
  const busy = view.inFlight.includes(actionKey.displayPairingApprove(pairing.pairing))
    || view.inFlight.includes(actionKey.displayPairingReject(pairing.pairing));
  return <Card>
    <CardHeader
      header={<Text weight="semibold">{platformName(pairing.platform)} · {pairing.build}</Text>}
      action={<Badge label="Verify on TV" />} />
    <p className={styles.phrase}>{pairing.confirmationPhrase.join("  ")}</p>
    <Fact label="Certificate SHA-256" value={pairing.certificateSha256} />
    <CardFooter className={styles.end}>
      <Button appearance="secondary" disabled={busy}
        onClick={() => void dispatch({ type: "displayPairingReject", pairing: pairing.pairing })}>Reject</Button>
      <Button appearance="primary" disabled={busy} onClick={onApprove}>Approve…</Button>
    </CardFooter>
  </Card>;
}

function ReceiverCard({ receiver, assignment, display, view, openDialog }: {
  receiver: DisplayReceiver; assignment: DisplayAssignment | undefined; display: Display; view: ClientView;
  openDialog(dialog: DisplaysDialog): void;
}) {
  const styles = useStyles();
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
  return <Card>
    <CardHeader
      header={<Text weight="semibold">{receiver.label}</Text>}
      description={<Caption1 className={styles.muted}>{platformName(receiver.platform)} · {receiver.build}</Caption1>}
      action={<Badge label={revoked ? "Revoked" : health === null ? "Not yet reported" : words(health.connection)}
        solid={!revoked && health !== null} />} />
    {assignment === undefined
      ? <Body1 className={styles.muted}>Unassigned</Body1>
      : <div className={styles.stack}>
        <Text weight="semibold">{assignment.world} · {assignment.surface}</Text>
        <Caption1 className={styles.muted}>Orbit {shortId(assignment.orbit)} · program {shortId(assignment.program)}</Caption1>
        {assignment.syncGroup !== null && <Caption1 className={styles.muted}>
          Sync {assignment.syncGroup} · {assignment.syncMode === null ? "" : syncModeNames[assignment.syncMode]} ·{" "}
          {assignment.staticDelayMs >= 0 ? "+" : ""}{assignment.staticDelayMs} ms
        </Caption1>}
      </div>}
    {health !== null && <div className={styles.stack}>
      <Caption1 className={styles.muted}>{words(health.playback)} · item {shortId(health.currentItem)} · {health.elapsedMs} ms</Caption1>
      {assignment?.syncGroup != null && <Caption1 className={styles.muted}>
        Residual {health.driftResidualMs} ms · {health.correctionEvents} corrections
      </Caption1>}
      {health.lastError !== "none" && <Caption1 className={styles.danger}>Receiver reports {words(health.lastError)}</Caption1>}
    </div>}
    <CardFooter>
      {!revoked && <Button appearance="secondary" disabled={assigning || cannotAssign !== null}
        title={cannotAssign ?? undefined}
        onClick={() => openDialog({ kind: "assign", receiver })}>{assignment === undefined ? "Assign…" : "Replace…"}</Button>}
      {assignment !== undefined && <Button appearance="subtle" disabled={revokingAssignment}
        onClick={() => openDialog({ kind: "unassign", assignment })}>Unassign</Button>}
      {!revoked && <Button appearance="subtle" className={styles.danger} disabled={revokingDevice}
        onClick={() => openDialog({ kind: "revoke", receiver })}>Revoke receiver…</Button>}
    </CardFooter>
  </Card>;
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
        <Button appearance="secondary" onClick={onDismiss}>Cancel</Button>
        <Button appearance="primary" onClick={() => {
          void dispatch({ type: "displayAssignmentRevoke", assignment: dialog.assignment.assignment });
          onDismiss();
        }}>Unassign</Button>
      </DialogFooter>
    </AppDialog>;
    case "revoke": return <AppDialog title={`Revoke ${dialog.receiver.label}?`}
      description="Its proof key will stop working immediately. Reconnecting this receiver requires a new pairing ceremony."
      onDismiss={onDismiss}>
      <DialogFooter>
        <Button appearance="secondary" onClick={onDismiss}>Cancel</Button>
        <Button appearance="primary" onClick={() => {
          void dispatch({ type: "displayDeviceRevoke", device: dialog.receiver.device });
          onDismiss();
        }}>Revoke receiver</Button>
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
  const problem = entered !== "" && !long
    ? `At least ${minPassphrase} characters.`
    : again !== "" && !matches ? "These do not match." : undefined;
  return <AppDialog title="Add a passphrase"
    description="A second way into the identifier key, independent of this machine and this identity. It is not stored — it wraps the key and is forgotten, so losing it costs this path and nothing else."
    onDismiss={onDismiss}>
    <Field label="Passphrase"><Input type="password" value={entered} onChange={(_, data) => setEntered(data.value)} /></Field>
    {/* Typed twice because it cannot be recovered and cannot be shown back:
        a mistyped passphrase would look like a working slot until the day it
        was the only one left. */}
    <Field label="Again" validationMessage={problem} validationState={problem === undefined ? "none" : "warning"}>
      <Input type="password" value={again} onChange={(_, data) => setAgain(data.value)} />
    </Field>
    <DialogFooter>
      <Button appearance="secondary" onClick={onDismiss}>Cancel</Button>
      <Button appearance="primary" disabled={!long || !matches} onClick={() => {
        void dispatch({ type: "displayIdentifierAdmitPassphrase", passphrase: entered });
        onDismiss();
      }}>Add it</Button>
    </DialogFooter>
  </AppDialog>;
}

function ApproveDialog({ pairing, dispatch, onDismiss }: { pairing: DisplayPairing; dispatch: Dispatch; onDismiss(): void }) {
  const styles = useStyles();
  const [label, setLabel] = useState(platformName(pairing.platform));
  return <AppDialog title="Approve this display?"
    description="Continue only if the six words and certificate fingerprint exactly match the receiver screen."
    onDismiss={onDismiss}>
    <p className={styles.phrase}>{pairing.confirmationPhrase.join("  ")}</p>
    <Field label="Display name"><Input value={label} onChange={(_, data) => setLabel(data.value)} /></Field>
    <DialogFooter>
      <Button appearance="secondary" onClick={onDismiss}>Cancel</Button>
      <Button appearance="primary" disabled={label.trim() === ""} onClick={() => {
        void dispatch({ type: "displayPairingApprove", pairing: pairing.pairing, label: label.trim() });
        onDismiss();
      }}>Approve</Button>
    </DialogFooter>
  </AppDialog>;
}

/** The assignment form: what to show, where from, and how it goes stale. */
function AssignmentDraftFields({ draft, setDraft, surfaces, orbits }: {
  draft: AssignmentDraft; setDraft(next: AssignmentDraft): void; surfaces: DisplaySurface[]; orbits: Orbit[];
}) {
  const styles = useStyles();
  const chosen = surfaces.find((surface) => surfaceKey(surface) === draft.chosenKey);
  const signage = chosen !== undefined && isSignageSurface(chosen);
  const problem = draft.input.trim() === "" ? null : inputProblem(draft, surfaces);
  const set = <K extends keyof AssignmentDraft>(key: K, value: AssignmentDraft[K]) => setDraft({ ...draft, [key]: value });
  return <>
    <Field label="Orbit"><Select value={draft.orbit} onChange={(_, data) => set("orbit", data.value)}>
      {orbits.map((row) => <option key={row.space} value={row.space}>{row.name}</option>)}
    </Select></Field>
    <Field label="Display surface"><Select value={draft.chosenKey} onChange={(_, data) => setDraft({ ...draft, chosenKey: data.value, input: "" })}>
      {surfaces.map((surface) => <option key={surfaceKey(surface)} value={surfaceKey(surface)}>
        {surface.title} · {surface.world}
      </option>)}
    </Select></Field>
    <Field label={signage ? "Signage program body ID" : "Package input JSON"}
      validationMessage={problem ?? undefined} validationState={problem === null ? "none" : "warning"}>
      {signage
        ? <Input value={draft.input} onChange={(_, data) => set("input", data.value)} />
        : <Textarea rows={4} resize="vertical" value={draft.input} onChange={(_, data) => set("input", data.value)} />}
    </Field>
    <div className={styles.grid2}>
      <Field label="Theme"><Select value={draft.theme} onChange={(_, data) => set("theme", data.value as DisplayTheme)}>
        {(Object.keys(themeNames) as DisplayTheme[]).map((option) => <option key={option} value={option}>{themeNames[option]}</option>)}
      </Select></Field>
      <Field label="Stale after (seconds)"><Input value={draft.staleSeconds} onChange={(_, data) => set("staleSeconds", data.value)} /></Field>
    </div>
    <Field label="When stale"><Select value={draft.onStale} onChange={(_, data) => set("onStale", data.value as DisplayStaleAction)}>
      {(Object.keys(staleActionNames) as DisplayStaleAction[]).map((option) =>
        <option key={option} value={option}>{staleActionNames[option]}</option>)}
    </Select></Field>
    <Field label="Sync group (optional)"><Input value={draft.syncGroup} onChange={(_, data) => set("syncGroup", data.value)} /></Field>
    <div className={styles.grid2}>
      <Field label="Sync mode"><Select value={draft.syncMode} onChange={(_, data) => set("syncMode", data.value as DisplaySyncMode)}>
        {(Object.keys(syncModeNames) as DisplaySyncMode[]).map((option) =>
          <option key={option} value={option}>{syncModeNames[option]}</option>)}
      </Select></Field>
      <Field label="Static delay (ms, + advances)"><Input value={draft.staticDelay} onChange={(_, data) => set("staticDelay", data.value)} /></Field>
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
      <Button appearance="secondary" onClick={onDismiss}>Cancel</Button>
      <Button appearance="primary" disabled={payload === null} onClick={assign}>Assign</Button>
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
  const styles = useStyles();
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
    <Field label="Display name"><Input value={label} placeholder="Lobby" autoFocus onChange={(_, data) => setLabel(data.value)} /></Field>
    {canPromise
      ? <Checkbox label="Show something as soon as it connects" checked={promise} onChange={(_, data) => setPromise(data.checked === true)} />
      : <Caption1 className={mergeClasses(styles.muted)}>{display.surfaces.length === 0
          ? "No selected World declares a display surface, so the display will enrol and wait for an assignment."
          : "This identity has no Orbit to draw from, so the display will enrol and wait for an assignment."}</Caption1>}
    {promise && canPromise && <AssignmentDraftFields draft={draft} setDraft={setDraft} surfaces={display.surfaces} orbits={orbits} />}
    <DialogFooter>
      <Button appearance="secondary" onClick={onDismiss}>Cancel</Button>
      <Button appearance="primary" disabled={!ready} onClick={mint}>Get a code</Button>
    </DialogFooter>
  </AppDialog>;
}
