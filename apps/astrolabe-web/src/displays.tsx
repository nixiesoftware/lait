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
  type Orbit,
} from "./client";
import { AppDialog, DialogFooter, Empty, Fact, Notice, SectionTitle, words } from "./kit";

type Dispatch = (action: ClientAction) => Promise<void>;

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
  // Signage first when it is there: it is the surface a TV most often shows,
  // and the one whose input is a single id rather than JSON.
  const first = surfaces.find(isSignageSurface) ?? surfaces[0];
  return {
    orbit: orbits[0]?.space ?? "",
    chosenKey: first === undefined ? "" : surfaceKey(first),
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

/** A World added from an unsealed tree, beside the release it was copied from. */
export function isLocalWorld(surface: Pick<DisplaySurface, "world">): boolean {
  return surface.world.startsWith("local.");
}

/** What the person is asked for, in their words, for each surface. */
export function inputPrompt(surface: Pick<DisplaySurface, "world" | "surface">): { label: string; hint: string; json: boolean } {
  if (isSignageSurface(surface)) return { label: "Screen", hint: "The screen's id, from the Signage app.", json: false };
  if (isIssuesBoard(surface)) return { label: "Project", hint: "The project's key, for example ENG.", json: false };
  return { label: "Package input JSON", hint: "This surface takes its own JSON input.", json: true };
}

/** A surface's name as a choice: "A Signage screen", "An Issues board" — and which copy, when there are two. */
export function surfaceChoice(surface: DisplaySurface): string {
  const name = isSignageSurface(surface) ? "A Signage screen" : isIssuesBoard(surface) ? "An Issues board" : surface.title;
  return isLocalWorld(surface) ? `${name} (local copy)` : name;
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

/** The listing taken for this surface in this Space, if one has been. */
export function choicesFor(
  listings: DisplayChoices[], orbit: string, surface: Pick<DisplaySurface, "world" | "surface">,
): DisplayChoices | undefined {
  return listings.find((listed) => listed.orbit === orbit && listed.world === surface.world && listed.surface === surface.surface);
}

/**
 * How the "which one" field is drawn: a picker when the World listed its
 * choices, a typed field otherwise — with the hint saying which case this is.
 * "Nothing to show yet" and "could not list" are different facts; only the
 * second is worth a typed id.
 */
export function chooser(
  listed: DisplayChoices | undefined, looking: boolean, prompt: { label: string; hint: string },
): { kind: "pick"; choices: DisplayChoices["choices"] & object } | { kind: "type"; hint: string } {
  if (listed?.choices !== null && listed?.choices !== undefined) {
    if (listed.choices.length > 0) return { kind: "pick", choices: listed.choices };
    return { kind: "type", hint: `Nothing to show in this Space yet. ${prompt.hint}` };
  }
  if (listed !== undefined) return { kind: "type", hint: `Couldn't list them (${listed.unavailable ?? "no reason given"}). ${prompt.hint}` };
  if (looking) return { kind: "type", hint: `Looking up what there is… ${prompt.hint}` };
  return { kind: "type", hint: prompt.hint };
}

/**
 * Why the draft's input cannot cross yet, or null when it can. The daemon's
 * parser is the one that would otherwise say so — from the other window,
 * after the fact.
 */
export function inputProblem(draft: Pick<AssignmentDraft, "chosenKey" | "input">, surfaces: DisplaySurface[]): string | null {
  const chosen = surfaces.find((surface) => surfaceKey(surface) === draft.chosenKey);
  if (chosen === undefined) return "Choose what to show.";
  const value = draft.input.trim();
  const prompt = inputPrompt(chosen);
  if (value === "") return `Enter the ${prompt.label.toLowerCase()}.`;
  if (!prompt.json) return null;
  try {
    JSON.parse(value);
    return null;
  } catch {
    return "The package input must be JSON — for example {\"project\":\"ENG\"}.";
  }
}

/** The draft as the daemon takes it, or null while it could not cross. */
export function assignmentPayload(draft: AssignmentDraft, surfaces: DisplaySurface[]): DisplayAssignmentRequest | null {
  const chosen = surfaces.find((surface) => surfaceKey(surface) === draft.chosenKey);
  if (chosen === undefined || !assignmentDraftValid(draft) || inputProblem(draft, surfaces) !== null) return null;
  return {
    orbit: draft.orbit,
    world: chosen.world,
    surface: chosen.surface,
    inputJson: surfaceInput(chosen, draft.input.trim()),
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

export type Tone = "good" | "neutral" | "warn" | "crit";

/** One chip carries the whole health story of a TV. */
export function tvStatus(receiver: Pick<DisplayReceiver, "revokedAtUnixMs" | "health">): { label: string; tone: Tone } {
  if (receiver.revokedAtUnixMs !== null) return { label: "Removed", tone: "neutral" };
  if (receiver.health === null) return { label: "Connecting…", tone: "neutral" };
  switch (receiver.health.connection) {
    case "online": return { label: "Connected", tone: "good" };
    case "retrying": return { label: "Reconnecting…", tone: "warn" };
    case "offline": return { label: "Offline", tone: "crit" };
    default: return { label: words(receiver.health.connection), tone: "neutral" };
  }
}

/** The surface a TV shows, by the name its World gives it. */
export function surfaceTitle(surfaces: DisplaySurface[], target: Pick<DisplayAssignment, "world" | "surface">): string {
  const found = surfaces.find((surface) => surface.world === target.world && surface.surface === target.surface);
  if (found !== undefined) return found.title;
  return `${target.world} · ${target.surface}`;
}

/** The second line of a TV's row. */
export function showingLine(assignment: DisplayAssignment | undefined, surfaces: DisplaySurface[]): string {
  return assignment === undefined ? "Nothing showing yet" : `Showing ${surfaceTitle(surfaces, assignment)}`;
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
  | { kind: "add" }
  | { kind: "approve"; pairing: DisplayPairing }
  | { kind: "assign"; receiver: DisplayReceiver }
  | { kind: "unassign"; assignment: DisplayAssignment; receiver: DisplayReceiver }
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
  // A code is a TV that is not here yet. Once it has enrolled, the TV's own
  // row says so; the code row leaves rather than saying it twice.
  const waiting = display?.pendingRendezvous.filter((minted) =>
    minted.state !== "connected" || !tvs.some((receiver) => receiver.device === minted.device)) ?? [];
  const nothingYet = tvs.length === 0 && waiting.length === 0;
  return <section className={styles.surface} aria-label="Displays">
    <header className={styles.header}>
      <Button appearance="subtle" icon={ownedWindow ? <Dismiss20Regular /> : <ArrowLeft20Regular />} onClick={onBack}>
        {ownedWindow ? "Close window" : "Library"}
      </Button>
      <div className={styles.headerCopy}>
        <Title3>Displays</Title3>
        <Caption1 className={styles.muted}>The TVs connected to this computer, and what each one shows.</Caption1>
      </div>
      <Button appearance="primary" icon={<Add20Regular />} disabled={display === null} onClick={() => setDialog({ kind: "add" })}>Add a TV</Button>
    </header>
    <div className={styles.scroll}>
      {display === null
        ? <div className={styles.column}><Empty said="Reading this computer's displays…" /></div>
        : <div className={styles.column}>
          {nothingYet && <div className={styles.hero}>
            <Tv20Regular className={styles.heroIcon} />
            <Text size={500} weight="semibold">Connect your first TV</Text>
            <Caption1 className={styles.muted}>Open Astrolabe on the TV, get a code here, enter it there. A minute, start to finish.</Caption1>
            <Button appearance="primary" icon={<Add20Regular />} onClick={() => setDialog({ kind: "add" })}>Add a TV</Button>
          </div>}
          {waiting.length > 0 && <section>
            <SectionTitle label="Waiting to connect" count={waiting.length} />
            <div className={styles.cards}>
              {waiting.map((minted) => <WaitingRow key={minted.rendezvous} minted={minted} surfaces={display.surfaces} view={view} dispatch={dispatch} />)}
            </div>
          </section>}
          {tvs.length > 0 && <section>
            <SectionTitle label="Your TVs" count={tvs.length} />
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

/** A code waiting to be entered, as the TV it is about to be. */
function WaitingRow({ minted, surfaces, view, dispatch }: { minted: DisplayRendezvous; surfaces: DisplaySurface[]; view: ClientView; dispatch: Dispatch }) {
  const styles = useStyles();
  const busy = view.inFlight.includes(actionKey.displayRendezvousRevoke(minted.rendezvous));
  const left = minutesLeft(minted.expiresAtUnixMs);
  const entry = codeEntry(minted);
  const status = minted.state === "connecting"
    ? { label: "Connecting…", tone: "neutral" as Tone }
    : left === 0 ? { label: "Code expired", tone: "crit" as Tone } : { label: "Waiting for the TV", tone: "warn" as Tone };
  return <Card className={styles.brandCard}>
    <div className={styles.row}>
      <div className={styles.rowIcon}><Tv20Regular /></div>
      <div className={styles.rowCopy}>
        <div className={styles.rowTitle}><Text weight="semibold">{minted.label}</Text><Chip {...status} /></div>
        <Caption1 className={styles.muted}>
          {minted.state === "connecting"
            ? "The TV entered the code and is finishing up."
            : left === 0
              ? "Nothing entered it in time. Cancel this one and get a new code."
              : <>On the TV, open Astrolabe and enter <span className={styles.codeInline}>{entry}</span> · {left} min left</>}
          {minted.assignment !== null && <> · will show <b>{surfaceTitle(surfaces, minted.assignment)}</b></>}
        </Caption1>
      </div>
      <div className={styles.rowActions}>
        {minted.state === "waiting" && left > 0 && <Button appearance="secondary" icon={<Copy20Regular />} onClick={() => void navigator.clipboard.writeText(entry)}>Copy code</Button>}
        <Menu>
          <MenuTrigger disableButtonEnhancement>
            <Button appearance="subtle" icon={<MoreHorizontal20Regular />} aria-label="More" />
          </MenuTrigger>
          <MenuPopover><MenuList>
            <MenuItem disabled={busy} onClick={() => void dispatch({ type: "displayRendezvousRevoke", rendezvous: minted.rendezvous })}>
              {minted.state === "waiting" ? "Cancel code" : "Dismiss"}
            </MenuItem>
          </MenuList></MenuPopover>
        </Menu>
      </div>
    </div>
  </Card>;
}

function TvRow({ receiver, assignment, display, view, openDialog }: {
  receiver: DisplayReceiver; assignment: DisplayAssignment | undefined; display: Display; view: ClientView;
  openDialog(dialog: DisplaysDialog): void;
}) {
  const styles = useStyles();
  const revoked = receiver.revokedAtUnixMs !== null;
  const status = tvStatus(receiver);
  const assigning = view.inFlight.includes(actionKey.displayAssignmentPut(receiver.device));
  const removing = view.inFlight.includes(actionKey.displayDeviceRevoke(receiver.device));
  const cannotAssign = view.orbits.length === 0
    ? "This identity has no Space to draw from yet."
    : display.surfaces.length === 0
      ? "No installed World offers anything a TV can show."
      : null;
  const refused = failureOf(view.failures, actionKey.displayAssignmentPut(receiver.device))
    ?? failureOf(view.failures, actionKey.displayDeviceRevoke(receiver.device));
  return <Card>
    <div className={styles.row}>
      <div className={styles.rowIcon}><Tv20Regular /></div>
      <div className={styles.rowCopy}>
        <div className={styles.rowTitle}><Text weight="semibold">{receiver.label}</Text><Chip {...status} /></div>
        <Caption1 className={styles.muted}>
          {revoked ? `Removed · ${platformName(receiver.platform)}` : showingLine(assignment, display.surfaces)}
          {!revoked && receiver.health !== null && receiver.health.lastError !== "none" && <span className={styles.crit}> · reports {words(receiver.health.lastError)}</span>}
        </Caption1>
        {refused !== undefined && <Caption1 className={styles.crit}>{refused.error}</Caption1>}
      </div>
      {!revoked && <div className={styles.rowActions}>
        <Button appearance={assignment === undefined ? "primary" : "secondary"} disabled={assigning || cannotAssign !== null}
          title={cannotAssign ?? undefined} onClick={() => openDialog({ kind: "assign", receiver })}>
          {assignment === undefined ? "Choose what to show" : "Change what it shows"}
        </Button>
        <Menu>
          <MenuTrigger disableButtonEnhancement>
            <Button appearance="subtle" icon={<MoreHorizontal20Regular />} aria-label="More" />
          </MenuTrigger>
          <MenuPopover><MenuList>
            {assignment !== undefined && <MenuItem onClick={() => openDialog({ kind: "unassign", assignment, receiver })}>Stop showing</MenuItem>}
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
    case "add": return <AddTvDialog display={display} view={view} dispatch={dispatch} onDismiss={onDismiss} />;
    case "passphrase": return <PassphraseDialog dispatch={dispatch} onDismiss={onDismiss} />;
    case "approve": return <ApproveDialog pairing={dialog.pairing} dispatch={dispatch} onDismiss={onDismiss} />;
    case "assign": return <AssignDialog receiver={dialog.receiver} surfaces={display.surfaces} view={view}
      dispatch={dispatch} onDismiss={onDismiss} />;
    case "unassign": return <AppDialog title={`Stop showing on ${dialog.receiver.label}?`}
      description="The TV goes to its ready screen until you choose something else." onDismiss={onDismiss}>
      <DialogFooter>
        <Button appearance="secondary" onClick={onDismiss}>Keep showing</Button>
        <Button appearance="primary" onClick={() => {
          void dispatch({ type: "displayAssignmentRevoke", assignment: dialog.assignment.assignment });
          onDismiss();
        }}>Stop showing</Button>
      </DialogFooter>
    </AppDialog>;
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

/**
 * What to show, as a choice among the surfaces the installed Worlds offer,
 * with the one thing each needs beneath the chosen one. Timing and sync are
 * one sentence of defaults until opened.
 */
function ShowFields({ draft, setDraft, surfaces, orbits, allowNothing, view, dispatch }: {
  draft: AssignmentDraft; setDraft(next: AssignmentDraft): void; surfaces: DisplaySurface[]; orbits: Orbit[]; allowNothing: boolean;
  view: ClientView; dispatch: Dispatch;
}) {
  const styles = useStyles();
  const chosen = surfaces.find((surface) => surfaceKey(surface) === draft.chosenKey);
  const problem = draft.input.trim() === "" ? null : inputProblem(draft, surfaces);
  const set = <K extends keyof AssignmentDraft>(key: K, value: AssignmentDraft[K]) => setDraft({ ...draft, [key]: value });
  // Ask the World what there is to choose from, once per (Space, surface)
  // each time this form is opened: the answer lands in the view and the last
  // one shows meanwhile. Asked per opening rather than per answer held, so a
  // World that could not list last time — a runner since replaced — is asked
  // again the next time somebody comes to choose, not never.
  const listings = view.display?.choices ?? [];
  const listed = chosen === undefined ? undefined : choicesFor(listings, draft.orbit, chosen);
  const listingKey = chosen === undefined ? null : actionKey.displaySurfaceChoices(draft.orbit, chosen.world, chosen.surface);
  const looking = listingKey !== null && view.inFlight.includes(listingKey);
  const asked = useRef(new Set<string>());
  useEffect(() => {
    if (chosen === undefined || draft.orbit === "" || listingKey === null || asked.current.has(listingKey)) return;
    asked.current.add(listingKey);
    void dispatch({ type: "displaySurfaceChoices", orbit: draft.orbit, world: chosen.world, surface: chosen.surface });
  }, [chosen, draft.orbit, listingKey, dispatch]);
  const which = chosen === undefined ? null : chooser(listed, looking, inputPrompt(chosen));
  const stale = `${staleActionNames[draft.onStale]} after ${draft.staleSeconds}s offline`;
  const sync = draft.syncGroup.trim() === "" ? "not kept in step with other TVs" : `in step with "${draft.syncGroup.trim()}"`;
  return <>
    <Field label="What should it show?">
      <RadioGroup value={draft.chosenKey} onChange={(_, data) => setDraft({ ...draft, chosenKey: data.value, input: "" })} className={styles.choices}>
        {surfaces.map((surface) => <Radio key={surfaceKey(surface)} value={surfaceKey(surface)} label={surfaceChoice(surface)} />)}
        {allowNothing && <Radio value="" label="Nothing yet — just connect it" />}
      </RadioGroup>
    </Field>
    {chosen !== undefined && which !== null && <div className={styles.choiceDetail}>
      {which.kind === "pick"
        ? <Field label={inputPrompt(chosen).label}>
            <Select value={draft.input} onChange={(_, data) => set("input", data.value)}>
              <option value="">Choose one…</option>
              {which.choices.map((choice) => <option key={choice.id} value={choice.id}>{choice.title}</option>)}
            </Select>
          </Field>
        : <Field label={inputPrompt(chosen).label} hint={which.hint}
            validationMessage={problem ?? undefined} validationState={problem === null ? "none" : "warning"}>
            {inputPrompt(chosen).json
              ? <Textarea rows={3} resize="vertical" value={draft.input} onChange={(_, data) => set("input", data.value)} />
              : <Input value={draft.input} onChange={(_, data) => set("input", data.value)} />}
          </Field>}
    </div>}
    {orbits.length > 1 && <Field label="Space"><Select value={draft.orbit} onChange={(_, data) => set("orbit", data.value)}>
      {orbits.map((row) => <option key={row.space} value={row.space}>{row.name}</option>)}
    </Select></Field>}
    <Accordion collapsible>
      <AccordionItem value="timing">
        <AccordionHeader size="small"><Caption1 className={styles.muted}>If this computer goes offline: {stale.toLowerCase()} · {sync}</Caption1></AccordionHeader>
        <AccordionPanel>
          <div className={styles.stack}>
            <div className={styles.grid2}>
              <Field label="If this computer goes offline"><Select value={draft.onStale} onChange={(_, data) => set("onStale", data.value as DisplayStaleAction)}>
                {(Object.keys(staleActionNames) as DisplayStaleAction[]).map((option) => <option key={option} value={option}>{staleActionNames[option]}</option>)}
              </Select></Field>
              <Field label="After (seconds)"><Input value={draft.staleSeconds} onChange={(_, data) => set("staleSeconds", data.value)} /></Field>
            </div>
            <Field label="Theme"><Select value={draft.theme} onChange={(_, data) => set("theme", data.value as DisplayTheme)}>
              {(Object.keys(themeNames) as DisplayTheme[]).map((option) => <option key={option} value={option}>{themeNames[option]}</option>)}
            </Select></Field>
            <Field label="Keep in step with other TVs (group name, optional)"><Input value={draft.syncGroup} onChange={(_, data) => set("syncGroup", data.value)} /></Field>
            {draft.syncGroup.trim() !== "" && <div className={styles.grid2}>
              <Field label="How"><Select value={draft.syncMode} onChange={(_, data) => set("syncMode", data.value as DisplaySyncMode)}>
                {(Object.keys(syncModeNames) as DisplaySyncMode[]).map((option) => <option key={option} value={option}>{syncModeNames[option]}</option>)}
              </Select></Field>
              <Field label="This TV's offset (ms)"><Input value={draft.staticDelay} onChange={(_, data) => set("staticDelay", data.value)} /></Field>
            </div>}
          </div>
        </AccordionPanel>
      </AccordionItem>
    </Accordion>
  </>;
}

function AssignDialog({ receiver, surfaces, view, dispatch, onDismiss }: {
  receiver: DisplayReceiver; surfaces: DisplaySurface[]; view: ClientView; dispatch: Dispatch; onDismiss(): void;
}) {
  const orbits = view.orbits;
  const [draft, setDraft] = useState(() => newAssignmentDraft(surfaces, orbits));
  const payload = assignmentPayload(draft, surfaces);
  return <AppDialog title={`What should ${receiver.label} show?`} onDismiss={onDismiss}>
    <ShowFields draft={draft} setDraft={setDraft} surfaces={surfaces} orbits={orbits} allowNothing={false} view={view} dispatch={dispatch} />
    <DialogFooter>
      <Button appearance="secondary" onClick={onDismiss}>Cancel</Button>
      <Button appearance="primary" disabled={payload === null} onClick={() => {
        if (payload === null) return;
        void dispatch({ type: "displayAssignmentPut", device: receiver.device, ...payload });
        onDismiss();
      }}>Show it</Button>
    </DialogFooter>
  </AppDialog>;
}

/**
 * One dialog, three states: name it and say what it shows; the code, big,
 * with the steps for the other screen, watching for the TV; connected.
 * It is the feedback channel for the whole act, so a refusal lands here,
 * under the button that asked.
 */
function AddTvDialog({ display, view, dispatch, onDismiss }: {
  display: Display; view: ClientView; dispatch: Dispatch; onDismiss(): void;
}) {
  const styles = useStyles();
  const orbits = view.orbits;
  const canPromise = display.surfaces.length > 0 && orbits.length > 0;
  const [openedAt] = useState(() => Date.now());
  const [label, setLabel] = useState("");
  const [draft, setDraft] = useState(() => canPromise
    ? newAssignmentDraft(display.surfaces, orbits)
    : { ...newAssignmentDraft(display.surfaces, orbits), chosenKey: "" });
  const [asked, setAsked] = useState(false);
  const promising = draft.chosenKey !== "";
  const payload = promising ? assignmentPayload(draft, display.surfaces) : null;
  const ready = label.trim() !== "" && (!promising || payload !== null);
  const minting = view.inFlight.includes(actionKey.displayRendezvousMint);
  const refused = asked && !minting ? failureOf(view.failures, actionKey.displayRendezvousMint) : undefined;
  // The code this dialog minted: the newest under this name since it opened.
  const minted = asked
    ? display.pendingRendezvous
      .filter((candidate) => candidate.label === label.trim() && candidate.createdAtUnixMs >= openedAt - 60_000)
      .sort((a, b) => b.createdAtUnixMs - a.createdAtUnixMs)[0]
    : undefined;
  const mint = () => {
    if (!ready) return;
    setAsked(true);
    void dispatch({ type: "displayRendezvousMint", label: label.trim(), assignment: payload });
  };

  if (minted !== undefined && minted.state === "connected") {
    const receiver = display.devices.find((candidate) => candidate.device === minted.device);
    const assignment = receiver === undefined ? undefined : assignmentFor(display, receiver.device);
    return <AppDialog title={`${minted.label} is connected`} onDismiss={onDismiss}>
      <div className={styles.step}>STEP 3 OF 3 · CONNECTED</div>
      <div className={styles.done}>✓ {assignment === undefined
        ? (minted.assignment === null ? "Connected. Choose what it shows from its row." : "Connected. Setting up what it shows…")
        : `Showing ${surfaceTitle(display.surfaces, assignment)}`}</div>
      <Caption1 className={styles.muted}>It is under Your TVs now. If it goes dark, its row says why.</Caption1>
      <DialogFooter>
        <Button appearance="secondary" onClick={() => { setAsked(false); setLabel(""); }}>Add another</Button>
        <Button appearance="primary" onClick={onDismiss}>Done</Button>
      </DialogFooter>
    </AppDialog>;
  }

  if (asked && refused === undefined) {
    const left = minted === undefined ? null : minutesLeft(minted.expiresAtUnixMs);
    return <AppDialog title="Enter this code on the TV" onDismiss={onDismiss}>
      <div className={styles.step}>STEP 2 OF 3 · ON THE TV</div>
      {minted === undefined
        ? <div className={styles.wait}><Spinner size="tiny" /> Getting a code…</div>
        : <>
          <div className={styles.bigCode}>{codeEntry(minted)}</div>
          <Caption1 className={styles.muted} style={{ textAlign: "center" }}>
            {left === 0 ? "This code has expired." : `Expires in ${left} min`}
          </Caption1>
          <ol className={styles.steps}>
            <li>Open <b>Astrolabe</b> on the TV.</li>
            <li>Type the code where it asks. Capitals and dashes don't matter.</li>
            <li>Press OK.</li>
          </ol>
          {minted.state === "connecting"
            ? <div className={styles.wait}><Spinner size="tiny" /> The TV entered the code and is finishing up…</div>
            : left === 0
              ? <Caption1 className={styles.crit}>Nothing entered it in time.</Caption1>
              : <div className={styles.wait}><Spinner size="tiny" /> Waiting for the TV…</div>}
        </>}
      <DialogFooter>
        {minted !== undefined && left !== 0 && <Button appearance="secondary" icon={<Copy20Regular />}
          onClick={() => void navigator.clipboard.writeText(codeEntry(minted))}>Copy code</Button>}
        {minted !== undefined && left === 0 && <Button appearance="primary" onClick={() => {
          void dispatch({ type: "displayRendezvousRevoke", rendezvous: minted.rendezvous });
          mint();
        }}>Get a new code</Button>}
        <Button appearance="subtle" onClick={onDismiss}>Close, keep waiting</Button>
      </DialogFooter>
    </AppDialog>;
  }

  return <AppDialog title="Add a TV"
    description="You'll get a code to enter on the TV. It works once and lasts 15 minutes."
    onDismiss={onDismiss}>
    <div className={styles.step}>STEP 1 OF 3 · NAME AND CONTENT</div>
    <Field label="Name"><Input value={label} placeholder="Lobby" autoFocus onChange={(_, data) => setLabel(data.value)} /></Field>
    {canPromise
      ? <ShowFields draft={draft} setDraft={setDraft} surfaces={display.surfaces} orbits={orbits} allowNothing view={view} dispatch={dispatch} />
      : <Caption1 className={mergeClasses(styles.muted)}>{display.surfaces.length === 0
          ? "No installed World offers anything a TV can show yet, so this TV will connect and wait."
          : "This identity has no Space to draw from yet, so this TV will connect and wait."}</Caption1>}
    {refused !== undefined && <Notice tone="danger">{refused.error}</Notice>}
    <DialogFooter>
      <Button appearance="secondary" onClick={onDismiss}>Cancel</Button>
      <Button appearance="primary" disabled={!ready || minting} onClick={mint}>Get a code</Button>
    </DialogFooter>
  </AppDialog>;
}
