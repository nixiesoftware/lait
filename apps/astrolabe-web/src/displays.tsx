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
import {
  Accordion,
  AccordionHeader,
  AccordionItem,
  AccordionPanel,
  Badge as FluentBadge,
  Body1,
  Button,
  Caption1,
  Card,
  CardFooter,
  CardHeader,
  Field,
  Input,
  Menu,
  MenuItem,
  MenuList,
  MenuPopover,
  MenuTrigger,
  Radio,
  RadioGroup,
  Select,
  Spinner,
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
  Copy20Regular,
  Dismiss20Regular,
  MoreHorizontal20Regular,
  Tv20Regular,
} from "@fluentui/react-icons";
import { useEffect, useRef, useState } from "react";

import {
  actionKey,
  type ClientAction,
  type ClientView,
  type Display,
  type DisplayAssignment,
  type DisplayAssignmentRequest,
  type DisplayChoices,
  type DisplayPairing,
  type DisplayReceiver,
  type DisplayRendezvous,
  type DisplayStaleAction,
  type DisplaySurface,
  type DisplaySyncMode,
  type DisplayTheme,
  type Failure,
  type LibraryWorld,
  type Orbit,
} from "./client";
import { AppDialog, DialogFooter, Empty, Fact, Notice, SectionTitle, words } from "./kit";

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



const surfaceKey = (surface: DisplaySurface) => `${surface.world} ${surface.surface}`;


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

const themeNames: Record<DisplayTheme, string> = { light: "Light", dark: "Dark", highContrast: "High contrast" };
const staleActionNames: Record<DisplayStaleAction, string> = { keepWithNativeBanner: "Keep the last picture", blank: "Go blank" };
const syncModeNames: Record<DisplaySyncMode, string> = { stayInSync: "Change items together", positional: "Match position within items" };

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
  scroll: { overflowY: "auto", padding: `${tokens.spacingVerticalL} ${tokens.spacingHorizontalXL} ${tokens.spacingVerticalXXL}` },
  column: { maxWidth: "880px", margin: "0 auto", display: "grid", gap: tokens.spacingVerticalM },
  cards: { display: "grid", gap: tokens.spacingVerticalS },
  row: { display: "grid", gridTemplateColumns: "auto minmax(0, 1fr) auto", gap: tokens.spacingHorizontalM, alignItems: "center" },
  rowIcon: { color: tokens.colorNeutralForeground3, display: "grid", placeItems: "center", width: "28px" },
  rowCopy: { display: "grid", gap: tokens.spacingVerticalXXS, minWidth: 0 },
  rowTitle: { display: "flex", alignItems: "center", gap: tokens.spacingHorizontalS, flexWrap: "wrap" },
  rowActions: { display: "flex", alignItems: "center", gap: tokens.spacingHorizontalXS, flexShrink: 0 },
  hero: {
    display: "grid",
    justifyItems: "center",
    gap: tokens.spacingVerticalS,
    textAlign: "center",
    padding: `${tokens.spacingVerticalXXXL} ${tokens.spacingHorizontalXL}`,
    border: `1px dashed ${tokens.colorNeutralStroke2}`,
    borderRadius: tokens.borderRadiusLarge,
  },
  heroIcon: { color: tokens.colorBrandForeground1, fontSize: "40px" },
  stack: { display: "grid", gap: tokens.spacingVerticalXS },
  muted: { color: tokens.colorNeutralForeground3 },
  crit: { color: tokens.colorPaletteRedForeground1 },
  chip: { whiteSpace: "nowrap", flexShrink: 0 },
  codeInline: { fontFamily: tokens.fontFamilyMonospace, fontWeight: tokens.fontWeightSemibold, letterSpacing: "0.06em", userSelect: "all" },
  phrase: {
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase400,
    fontWeight: tokens.fontWeightSemibold,
    color: tokens.colorBrandForeground1,
    letterSpacing: "0.04em",
  },
  facts: { display: "grid", gap: tokens.spacingVerticalXXS },
  foot: { marginTop: tokens.spacingVerticalM },
  // The dialog's three states.
  step: { fontFamily: tokens.fontFamilyMonospace, fontSize: tokens.fontSizeBase100, letterSpacing: "0.12em", color: tokens.colorBrandForeground1 },
  choices: { display: "grid", gap: tokens.spacingVerticalXS },
  choiceDetail: { paddingLeft: "28px" },
  bigCode: {
    textAlign: "center",
    padding: `${tokens.spacingVerticalL} 0 ${tokens.spacingVerticalS}`,
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeHero800,
    fontWeight: tokens.fontWeightSemibold,
    letterSpacing: "0.14em",
    userSelect: "all",
  },
  steps: { margin: 0, paddingLeft: "22px", color: tokens.colorNeutralForeground2, display: "grid", gap: tokens.spacingVerticalXXS },
  wait: { display: "flex", alignItems: "center", gap: tokens.spacingHorizontalS, color: tokens.colorNeutralForeground2 },
  done: {
    display: "flex",
    alignItems: "center",
    gap: tokens.spacingHorizontalS,
    padding: `${tokens.spacingVerticalS} ${tokens.spacingHorizontalM}`,
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: tokens.colorPaletteGreenBackground1,
    color: tokens.colorPaletteGreenForeground1,
    fontWeight: tokens.fontWeightSemibold,
  },
  grid2: { display: "grid", gridTemplateColumns: "repeat(2, minmax(0, 1fr))", gap: tokens.spacingHorizontalM },
  brandCard: { ...shorthands.borderColor(tokens.colorBrandStroke1) },
});

function Chip({ label, tone }: { label: string; tone: Tone }) {
  const styles = useStyles();
  const color = tone === "good" ? "success" : tone === "warn" ? "warning" : tone === "crit" ? "danger" : "informative";
  return <FluentBadge appearance="tint" color={color} size="medium" shape="rounded" className={styles.chip}>{label}</FluentBadge>;
}

type DisplaysDialog =
  | { kind: "approve"; pairing: DisplayPairing }
  | { kind: "revoke"; receiver: DisplayReceiver }
  | { kind: "passphrase" };

export function DisplaysSurface({ view, dispatch, onBack, ownedWindow = false }: {
  view: ClientView; dispatch: Dispatch; onBack(): void; ownedWindow?: boolean;
}) {
  const styles = useStyles();
  const [dialog, setDialog] = useState<DisplaysDialog | null>(null);
  const display = view.display;
  const tvs = display?.devices.filter((receiver) => receiver.revokedAtUnixMs === null) ?? [];
  const removed = display?.devices.filter((receiver) => receiver.revokedAtUnixMs !== null) ?? [];
  const nothingYet = tvs.length === 0;
  return <section className={styles.surface} aria-label="Linked TVs">
    <header className={styles.header}>
      <Button appearance="subtle" icon={ownedWindow ? <Dismiss20Regular /> : <ArrowLeft20Regular />} onClick={onBack}>
        {ownedWindow ? "Close window" : "Library"}
      </Button>
      <div className={styles.headerCopy}>
        <Title3>Linked TVs</Title3>
        <Caption1 className={styles.muted}>The TVs linked to this computer, and which World each one belongs to. What a TV shows is decided in that World.</Caption1>
      </div>
    </header>
    <div className={styles.scroll}>
      {display === null
        ? <div className={styles.column}><Empty said="Reading this computer's displays…" /></div>
        : <div className={styles.column}>
          {nothingYet && <div className={styles.hero}>
            <Tv20Regular className={styles.heroIcon} />
            <Text size={500} weight="semibold">No TV is linked yet</Text>
            <Caption1 className={styles.muted}>Add a TV from the World that will show it — in Signage, open a screen and choose Add a TV. A TV that enters just this site's name appears below, to approve by words.</Caption1>
          </div>}
          {tvs.length > 0 && <section>
            <SectionTitle label="Linked TVs" count={tvs.length} />
            <div className={styles.cards}>
              {tvs.map((receiver) => <TvRow key={receiver.device} receiver={receiver}
                assignment={assignmentFor(display, receiver.device)} display={display} view={view} openDialog={setDialog} />)}
            </div>
          </section>}
          <Accordion collapsible multiple className={styles.foot}>
            <AccordionItem value="words">
              <AccordionHeader>A TV is asking to connect by words {display.pendingPairings.length > 0 ? `(${display.pendingPairings.length})` : ""}</AccordionHeader>
              <AccordionPanel>
                {display.pendingPairings.length === 0
                  ? <Caption1 className={styles.muted}>None right now. A TV that enters just its site name shows six words instead of taking a code; when one does, it appears here for you to compare and approve.</Caption1>
                  : <div className={styles.cards}>
                    {display.pendingPairings.map((pairing) => <PairingCard key={pairing.pairing} pairing={pairing} view={view}
                      dispatch={dispatch} onApprove={() => setDialog({ kind: "approve", pairing })} />)}
                  </div>}
              </AccordionPanel>
            </AccordionItem>
            {removed.length > 0 && <AccordionItem value="removed">
              <AccordionHeader>Removed TVs ({removed.length})</AccordionHeader>
              <AccordionPanel>
                <div className={styles.cards}>
                  {removed.map((receiver) => <TvRow key={receiver.device} receiver={receiver} assignment={undefined} display={display} view={view} openDialog={setDialog} />)}
                </div>
              </AccordionPanel>
            </AccordionItem>}
            <AccordionItem value="connection">
              <AccordionHeader>Connection details</AccordionHeader>
              <AccordionPanel><ConnectionDetails display={display} /></AccordionPanel>
            </AccordionItem>
            <AccordionItem value="recovery">
              <AccordionHeader>Backup &amp; recovery</AccordionHeader>
              <AccordionPanel><Recovery custody={display.identifierCustody} onAddPassphrase={() => setDialog({ kind: "passphrase" })} /></AccordionPanel>
            </AccordionItem>
          </Accordion>
        </div>}
    </div>
    {dialog !== null && display !== null && <DisplaysDialogs dialog={dialog} display={display} view={view}
      dispatch={dispatch} onDismiss={() => setDialog(null)} />}
  </section>;
}


function TvRow({ receiver, assignment, display, view, openDialog }: {
  receiver: DisplayReceiver; assignment: DisplayAssignment | undefined; display: Display; view: ClientView;
  openDialog(dialog: DisplaysDialog): void;
}) {
  const styles = useStyles();
  const revoked = receiver.revokedAtUnixMs !== null;
  const now = useNow(15_000);
  const status = tvStatus(receiver, now);
  const removing = view.inFlight.includes(actionKey.displayDeviceRevoke(receiver.device));
  const refused = failureOf(view.failures, actionKey.displayDeviceRevoke(receiver.device));
  return <Card>
    <div className={styles.row}>
      <div className={styles.rowIcon}><Tv20Regular /></div>
      <div className={styles.rowCopy}>
        <div className={styles.rowTitle}><Text weight="semibold">{receiver.label}</Text><Chip {...status} /></div>
        <Caption1 className={styles.muted}>
          {revoked ? `Removed · ${platformName(receiver.platform)}` : heldBy(assignment, display.surfaces, view.library ?? [])}
          {!revoked && receiver.health !== null && receiver.health.lastError !== "none" && <span className={styles.crit}> · reports {words(receiver.health.lastError)}</span>}
        </Caption1>
        {refused !== undefined && <Caption1 className={styles.crit}>{refused.error}</Caption1>}
      </div>
      {!revoked && <div className={styles.rowActions}>
        <Menu>
          <MenuTrigger disableButtonEnhancement>
            <Button appearance="subtle" icon={<MoreHorizontal20Regular />} aria-label="More" />
          </MenuTrigger>
          <MenuPopover><MenuList>
            <MenuItem disabled={removing} onClick={() => openDialog({ kind: "revoke", receiver })}>Remove this TV…</MenuItem>
          </MenuList></MenuPopover>
        </Menu>
      </div>}
    </div>
  </Card>;
}

function PairingCard({ pairing, view, dispatch, onApprove }: {
  pairing: DisplayPairing; view: ClientView; dispatch: Dispatch; onApprove(): void;
}) {
  const styles = useStyles();
  const busy = view.inFlight.includes(actionKey.displayPairingApprove(pairing.pairing))
    || view.inFlight.includes(actionKey.displayPairingReject(pairing.pairing));
  return <Card>
    <CardHeader header={<Text weight="semibold">{platformName(pairing.platform)} · {pairing.build}</Text>}
      description={<Caption1 className={styles.muted}>Approve only if the TV shows these same six words.</Caption1>} />
    <p className={styles.phrase}>{pairing.confirmationPhrase.join("  ")}</p>
    <CardFooter>
      <Button appearance="primary" disabled={busy} onClick={onApprove}>The words match…</Button>
      <Button appearance="subtle" disabled={busy}
        onClick={() => void dispatch({ type: "displayPairingReject", pairing: pairing.pairing })}>Not this TV</Button>
    </CardFooter>
  </Card>;
}

function ConnectionDetails({ display }: { display: Display }) {
  const styles = useStyles();
  return <div className={styles.stack}>
    <Caption1 className={styles.muted}>What a TV app that takes a pairing file needs. A TV that enters a code never does.</Caption1>
    <div className={styles.facts}>
      <Fact label="This computer" value={display.label} />
      <Fact label="LAN origin" value={display.origin} />
      <Fact label="Certificate SHA-256" value={display.certificateSha256} />
    </div>
    <div><Button appearance="secondary" size="small" icon={<Copy20Regular />}
      onClick={() => void navigator.clipboard.writeText(receiverBootstrap(display))}>Copy pairing file</Button></div>
  </div>;
}

const slotNames: Record<string, string> = {
  "recovery-key": "this identity",
  "passphrase": "a passphrase",
  "windows-dpapi": "this Windows profile",
};

function Recovery({ custody, onAddPassphrase }: { custody: Display["identifierCustody"]; onAddPassphrase(): void }) {
  const styles = useStyles();
  if (custody === null) return <Caption1 className={styles.muted}>This computer does not report how its TV keys are protected.</Caption1>;
  const hasPassphrase = custody.slots.includes("passphrase");
  const paths = custody.slots.length === 0 ? "nothing" : custody.slots.map((slot) => slotNames[slot] ?? slot).join(", ");
  return <div className={styles.stack}>
    <Body1>{custody.portable
      ? "If this computer is lost, your TVs can be reconnected from another one."
      : "If this computer is lost, every TV will need connecting again — unless you set a recovery passphrase."}</Body1>
    <Caption1 className={styles.muted}>The key that names what your TVs show is unlocked by {paths}.</Caption1>
    {!hasPassphrase && <div><Button appearance="secondary" size="small" onClick={onAddPassphrase}>Set a recovery passphrase</Button></div>}
  </div>;
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
        <Button appearance="secondary" onClick={onDismiss}>Keep it</Button>
        <Button appearance="primary" onClick={() => {
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
    <Field label="Passphrase"><Input type="password" value={entered} onChange={(_, data) => setEntered(data.value)} /></Field>
    <Field label="Again" validationMessage={problem} validationState={problem === undefined ? "none" : "warning"}>
      <Input type="password" value={again} onChange={(_, data) => setAgain(data.value)} />
    </Field>
    <DialogFooter>
      <Button appearance="secondary" onClick={onDismiss}>Cancel</Button>
      <Button appearance="primary" disabled={!long || !matches} onClick={() => {
        void dispatch({ type: "displayIdentifierAdmitPassphrase", passphrase: entered });
        onDismiss();
      }}>Set it</Button>
    </DialogFooter>
  </AppDialog>;
}

function ApproveDialog({ pairing, dispatch, onDismiss }: { pairing: DisplayPairing; dispatch: Dispatch; onDismiss(): void }) {
  const styles = useStyles();
  const [label, setLabel] = useState(platformName(pairing.platform));
  return <AppDialog title="Connect this TV?"
    description="Continue only if the TV shows exactly these six words."
    onDismiss={onDismiss}>
    <p className={styles.phrase}>{pairing.confirmationPhrase.join("  ")}</p>
    <Field label="Name"><Input value={label} onChange={(_, data) => setLabel(data.value)} /></Field>
    <DialogFooter>
      <Button appearance="secondary" onClick={onDismiss}>Cancel</Button>
      <Button appearance="primary" disabled={label.trim() === ""} onClick={() => {
        void dispatch({ type: "displayPairingApprove", pairing: pairing.pairing, label: label.trim() });
        onDismiss();
      }}>Connect</Button>
    </DialogFooter>
  </AppDialog>;
}



