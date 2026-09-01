/**
 * Displays: the TVs connected to this computer, and what each one shows.
 *
 * Facts are the daemon's display projection. Local state is draft form input
 * only; approvals, assignments, and revocations all cross as actions and
 * return through the ordinary authoritative refresh.
 *
 * One job per screen. This window lists TVs and adds one; the dialog ends
 * holding a code and stays open until the TV arrives. Everything the daemon
 * knows that a person does not need — origins, fingerprints, key custody,
 * the words ceremony — survives as a disclosure at the foot, in the
 * daemon's own words, where it belongs.
 */
import { useEffect, useState } from "react";

import {
  actionKey,
  type ClientAction,
  type ClientView,
  type Display,
  type DisplayAssignment,
  type DisplayPairing,
  type DisplayReceiver,
  type DisplaySurface,
  type Failure,
  type LibraryWorld,
} from "./client";
import { AppDialog, Button, Card, Chip, DialogFooter, Disclosure, Empty, Fact, Field, PaneHead, RowMenu, SectionTitle, words } from "./kit";
import { IconCopy, IconTv } from "./icons";

type Dispatch = (action: ClientAction) => Promise<void>;

/** The clock, re-read every `everyMs`, so a "last seen" line ages without a view pump. */
function useNow(everyMs: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), everyMs);
    return () => clearInterval(timer);
  }, [everyMs]);
  return now;
}

/** The pinned receiver bootstrap a self-hosted TV app's setup screen accepts. */
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



/**
 * The signage surface takes a single screen id, not free JSON — the form
 * offers the one field and spells the wrapping itself.
 *
 * Known by its surface id, not its World's: a local copy of Signage serves
 * the same surface under `local.<handle>`, and it is the same form.
 */
export function isSignageSurface(surface: Pick<DisplaySurface, "world" | "surface">): boolean {
  return surface.surface === "signage.program";
}

/** The Issues board takes a project key, wrapped the same way. */
export function isIssuesBoard(surface: Pick<DisplaySurface, "world" | "surface">): boolean {
  return surface.surface === "issues.board.wall";
}


/** What the person is asked for, in their words, for each surface. */
export function inputPrompt(surface: Pick<DisplaySurface, "world" | "surface">): { label: string; hint: string; json: boolean } {
  if (isSignageSurface(surface)) return { label: "Screen", hint: "The screen's id, from the Signage app.", json: false };
  if (isIssuesBoard(surface)) return { label: "Project", hint: "The project's key, for example ENG.", json: false };
  return { label: "Package input JSON", hint: "This surface takes its own JSON input.", json: true };
}


/**
 * The key the Signage surface takes its screen under. The released surface
 * (contract 3) still calls a screen a `program`; this tree's (contract 4)
 * calls it a `screen`. Which one is talking is the surface's own declared
 * contract, so the answer is read from it rather than assumed from the build
 * this client came from — the installed World is the one that refuses.
 */
export function signageInputKey(surface: Pick<DisplaySurface, "contractVersion">): "screen" | "program" {
  return surface.contractVersion >= 4 ? "screen" : "program";
}

/** The surface's input as the package takes it: the id or key wrapped, or the JSON verbatim. */
export function surfaceInput(surface: Pick<DisplaySurface, "world" | "surface" | "contractVersion">, value: string): string {
  if (isSignageSurface(surface)) return JSON.stringify({ [signageInputKey(surface)]: value });
  if (isIssuesBoard(surface)) return JSON.stringify({ project: value });
  return value;
}







export type Tone = "good" | "neutral" | "warn" | "crit";

/** One chip carries the whole health story of a TV. */
/**
 * A running receiver reports every 30–55 seconds. Three minutes of silence
 * is a TV that is gone, whatever its last report said — the report cannot
 * say "and then I stopped", only the clock can.
 */
export const silentAfterMs = 3 * 60_000;
/** How long a fresh enrolment is "connecting" before its silence is called. */
export const connectingGraceMs = 2 * 60_000;

/** "4 min ago", "3 h ago", "2 days ago". */
export function agoLabel(ms: number): string {
  const minutes = Math.max(1, Math.round(ms / 60_000));
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours} h ago`;
  return `${Math.round(hours / 24)} days ago`;
}

/**
 * A TV's state in one chip. Health is held by the daemon in memory only, so
 * "no report" means none since this computer started listening: a fresh
 * enrolment is still connecting, an old one has not spoken — and a last
 * report older than the cadence allows is a TV that is gone, however
 * cheerful the report was.
 */
export function tvStatus(
  receiver: Pick<DisplayReceiver, "revokedAtUnixMs" | "health" | "issuedAtUnixMs">, now = Date.now(),
): { label: string; tone: Tone } {
  if (receiver.revokedAtUnixMs !== null) return { label: "Removed", tone: "neutral" };
  if (receiver.health === null) {
    return now - receiver.issuedAtUnixMs < connectingGraceMs
      ? { label: "Connecting…", tone: "neutral" }
      : { label: "Not heard from", tone: "warn" };
  }
  const { reportedAtUnixMs } = receiver.health;
  if (reportedAtUnixMs !== null && now - reportedAtUnixMs > silentAfterMs) {
    return { label: `Last seen ${agoLabel(now - reportedAtUnixMs)}`, tone: "warn" };
  }
  switch (receiver.health.connection) {
    case "online": return { label: "Connected", tone: "good" };
    case "retrying": return { label: "Reconnecting…", tone: "warn" };
    case "offline": return { label: "Offline", tone: "crit" };
    default: return { label: words(receiver.health.connection), tone: "neutral" };
  }
}

/**
 * Which World holds a TV — the one fact this manager states about what a TV
 * shows. The screen, the program, the schedule are that World's to say.
 */
export function heldBy(
  assignment: DisplayAssignment | undefined, surfaces: DisplaySurface[], library: Pick<LibraryWorld, "world" | "displayName">[],
): string {
  if (assignment === undefined) return "Not held by any World — a World can point it at something";
  const row = library.find((entry) => entry.world === assignment.world);
  const name = row === undefined
    ? surfaceTitle(surfaces, assignment)
    : assignment.world.startsWith("local.") ? `${row.displayName} (local copy)` : row.displayName;
  return `Held by ${name}`;
}

/** The surface a TV shows, by the name its World gives it. */
export function surfaceTitle(surfaces: DisplaySurface[], target: Pick<DisplayAssignment, "world" | "surface">): string {
  const found = surfaces.find((surface) => surface.world === target.world && surface.surface === target.surface);
  if (found !== undefined) return found.title;
  return `${target.world} · ${target.surface}`;
}


/** A failure of one action, if the view still carries it. */
export function failureOf(failures: Failure[], key: string): Failure | undefined {
  return failures.find((failure) => failure.key === key);
}

type DisplaysDialog =
  | { kind: "approve"; pairing: DisplayPairing }
  | { kind: "revoke"; receiver: DisplayReceiver }
  | { kind: "passphrase" };

export function DisplaysPane({ view, dispatch }: { view: ClientView; dispatch: Dispatch }) {
  const [dialog, setDialog] = useState<DisplaysDialog | null>(null);
  const display = view.display;
  const tvs = display?.devices.filter((receiver) => receiver.revokedAtUnixMs === null) ?? [];
  const removed = display?.devices.filter((receiver) => receiver.revokedAtUnixMs !== null) ?? [];
  const nothingYet = tvs.length === 0;
  return <>
    <div className="secondary-scroll" aria-label="Linked TVs">
      {display === null
        ? <div className="content-column">
          <PaneHead title="Linked TVs" />
          <Empty said="Reading this computer's displays…" />
        </div>
        : <div className="content-column">
          <PaneHead title="Linked TVs" />
          {nothingYet && <Empty icon={<IconTv size={40} />} said="No TV is linked yet"
            next="Add a TV from the World that will show it — in Signage, open a screen and choose Add a TV. A TV that enters just this site's name appears below, to approve by words." />}
          {tvs.length > 0 && <section>
            <SectionTitle label="Linked TVs" count={tvs.length} />
            <div className="card-stack">
              {tvs.map((receiver) => <TvRow key={receiver.device} receiver={receiver}
                assignment={assignmentFor(display, receiver.device)} display={display} view={view} openDialog={setDialog} />)}
            </div>
          </section>}
          <div>
            <Disclosure summary={<>A TV is asking to connect by words {display.pendingPairings.length > 0 ? `(${display.pendingPairings.length})` : ""}</>}>
              {display.pendingPairings.length === 0
                ? <small className="dim-line">None right now. A TV that enters just its site name shows six words instead of taking a code; when one does, it appears here for you to compare and approve.</small>
                : <div className="card-stack">
                  {display.pendingPairings.map((pairing) => <PairingCard key={pairing.pairing} pairing={pairing} view={view}
                    dispatch={dispatch} onApprove={() => setDialog({ kind: "approve", pairing })} />)}
                </div>}
            </Disclosure>
            {removed.length > 0 && <Disclosure summary={`Removed TVs (${removed.length})`}>
              <div className="card-stack">
                {removed.map((receiver) => <TvRow key={receiver.device} receiver={receiver} assignment={undefined} display={display} view={view} openDialog={setDialog} />)}
              </div>
            </Disclosure>}
            <Disclosure summary="Connection details"><ConnectionDetails display={display} /></Disclosure>
            <Disclosure summary="Backup &amp; recovery">
              <Recovery custody={display.identifierCustody} onAddPassphrase={() => setDialog({ kind: "passphrase" })} />
            </Disclosure>
          </div>
        </div>}
    </div>
    {dialog !== null && display !== null && <DisplaysDialogs dialog={dialog} display={display} view={view}
      dispatch={dispatch} onDismiss={() => setDialog(null)} />}
  </>;
}

function TvRow({ receiver, assignment, display, view, openDialog }: {
  receiver: DisplayReceiver; assignment: DisplayAssignment | undefined; display: Display; view: ClientView;
  openDialog(dialog: DisplaysDialog): void;
}) {
  const revoked = receiver.revokedAtUnixMs !== null;
  const now = useNow(15_000);
  const status = tvStatus(receiver, now);
  const removing = view.inFlight.includes(actionKey.displayDeviceRevoke(receiver.device));
  const refused = failureOf(view.failures, actionKey.displayDeviceRevoke(receiver.device));
  return <Card>
    <div className="item-row">
      <div className="item-icon"><IconTv /></div>
      <div className="item-copy">
        <div className="item-title"><strong>{receiver.label}</strong><Chip label={status.label} tone={status.tone} /></div>
        <small className="dim-line">
          {revoked ? `Removed · ${platformName(receiver.platform)}` : heldBy(assignment, display.surfaces, view.library ?? [])}
          {!revoked && receiver.health !== null && receiver.health.lastError !== "none" && <span className="danger-text"> · reports {words(receiver.health.lastError)}</span>}
        </small>
        {refused !== undefined && <small className="dim-line danger-text">{refused.error}</small>}
      </div>
      {!revoked && <div className="item-actions">
        <RowMenu items={[{ label: "Remove this TV…", onAction: () => openDialog({ kind: "revoke", receiver }), disabled: removing, danger: true }]} />
      </div>}
    </div>
  </Card>;
}

function PairingCard({ pairing, view, dispatch, onApprove }: {
  pairing: DisplayPairing; view: ClientView; dispatch: Dispatch; onApprove(): void;
}) {
  const busy = view.inFlight.includes(actionKey.displayPairingApprove(pairing.pairing))
    || view.inFlight.includes(actionKey.displayPairingReject(pairing.pairing));
  return <Card>
    <div className="item-copy">
      <div className="item-title"><strong>{platformName(pairing.platform)} · {pairing.build}</strong></div>
      <small className="dim-line">Approve only if the TV shows these same six words.</small>
    </div>
    <p className="phrase">{pairing.confirmationPhrase.join("  ")}</p>
    <div className="button-row">
      <Button variant="primary" disabled={busy} onPress={onApprove}>The words match…</Button>
      <Button variant="ghost" disabled={busy}
        onPress={() => void dispatch({ type: "displayPairingReject", pairing: pairing.pairing })}>Not this TV</Button>
    </div>
  </Card>;
}

function ConnectionDetails({ display }: { display: Display }) {
  return <>
    <small className="dim-line">What a TV app that takes a pairing file needs. A TV that enters a code never does.</small>
    <div>
      <Fact label="This computer" value={display.label} />
      <Fact label="LAN origin" value={display.origin} />
      <Fact label="Certificate SHA-256" value={display.certificateSha256} />
    </div>
    <div className="button-row"><Button variant="quiet"
      onPress={() => void navigator.clipboard.writeText(receiverBootstrap(display))}><IconCopy size={16} /> Copy pairing file</Button></div>
  </>;
}

const slotNames: Record<string, string> = {
  "recovery-key": "this identity",
  "passphrase": "a passphrase",
  "windows-dpapi": "this Windows profile",
};

function Recovery({ custody, onAddPassphrase }: { custody: Display["identifierCustody"]; onAddPassphrase(): void }) {
  if (custody === null) return <small className="dim-line">This computer does not report how its TV keys are protected.</small>;
  const hasPassphrase = custody.slots.includes("passphrase");
  const paths = custody.slots.length === 0 ? "nothing" : custody.slots.map((slot) => slotNames[slot] ?? slot).join(", ");
  return <>
    <p className="body-line">{custody.portable
      ? "If this computer is lost, your TVs can be reconnected from another one."
      : "If this computer is lost, every TV will need connecting again — unless you set a recovery passphrase."}</p>
    <small className="dim-line">The key that names what your TVs show is unlocked by {paths}.</small>
    {!hasPassphrase && <div className="button-row"><Button variant="quiet" onPress={onAddPassphrase}>Set a recovery passphrase</Button></div>}
  </>;
}

function DisplaysDialogs({ dialog, display, view, dispatch, onDismiss }: {
  dialog: DisplaysDialog; display: Display; view: ClientView; dispatch: Dispatch; onDismiss(): void;
}) {
  switch (dialog.kind) {
    case "passphrase": return <PassphraseDialog dispatch={dispatch} onDismiss={onDismiss} />;
    case "approve": return <ApproveDialog pairing={dialog.pairing} dispatch={dispatch} onDismiss={onDismiss} />;
    case "revoke": return <AppDialog title={`Remove ${dialog.receiver.label}?`}
      description="The TV stops immediately and forgets this computer. Connecting it again means a new code."
      onDismiss={onDismiss}>
      <DialogFooter>
        <Button variant="quiet" onPress={onDismiss}>Keep it</Button>
        <Button variant="danger" onPress={() => {
          void dispatch({ type: "displayDeviceRevoke", device: dialog.receiver.device });
          onDismiss();
        }}>Remove this TV</Button>
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
  return <AppDialog title="Set a recovery passphrase"
    description="A way back in that does not depend on this computer. It is not stored anywhere — losing it costs only this way in."
    onDismiss={onDismiss}>
    <Field label="Passphrase"><input type="password" value={entered} onChange={(event) => setEntered(event.target.value)} /></Field>
    <Field label="Again" error={problem}>
      <input type="password" value={again} onChange={(event) => setAgain(event.target.value)} />
    </Field>
    <DialogFooter>
      <Button variant="quiet" onPress={onDismiss}>Cancel</Button>
      <Button variant="primary" disabled={!long || !matches} onPress={() => {
        void dispatch({ type: "displayIdentifierAdmitPassphrase", passphrase: entered });
        onDismiss();
      }}>Set it</Button>
    </DialogFooter>
  </AppDialog>;
}

function ApproveDialog({ pairing, dispatch, onDismiss }: { pairing: DisplayPairing; dispatch: Dispatch; onDismiss(): void }) {
  const [label, setLabel] = useState(platformName(pairing.platform));
  return <AppDialog title="Connect this TV?"
    description="Continue only if the TV shows exactly these six words."
    onDismiss={onDismiss}>
    <p className="phrase">{pairing.confirmationPhrase.join("  ")}</p>
    <Field label="Name"><input value={label} onChange={(event) => setLabel(event.target.value)} /></Field>
    <DialogFooter>
      <Button variant="quiet" onPress={onDismiss}>Cancel</Button>
      <Button variant="primary" disabled={label.trim() === ""} onPress={() => {
        void dispatch({ type: "displayPairingApprove", pairing: pairing.pairing, label: label.trim() });
        onDismiss();
      }}>Connect</Button>
    </DialogFooter>
  </AppDialog>;
}
