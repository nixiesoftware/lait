/**
 * Your devices: every device that is you, and the one act that adds another.
 *
 * A device is added once, to the person — not once per Space. So this window
 * asks for exactly one thing: the code the other device is showing. What
 * comes back is six words to compare aloud, and confirming them is the whole
 * ceremony. Nothing here is per Space, and there is no second enrolment.
 *
 * Facts are the daemon's. The only local state is the code being typed and
 * which offer a person is looking at; every answer crosses as an action and
 * comes back as the whole view, so this side never holds an answer it was
 * handed.
 */
import {
  Badge as FluentBadge,
  Body1,
  Button,
  Caption1,
  Card,
  CardFooter,
  CardHeader,
  Field,
  Input,
  Text,
  Title3,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { ArrowLeft20Regular, Dismiss20Regular, Laptop20Regular } from "@fluentui/react-icons";
import { useEffect, useState } from "react";

import {
  actionKey,
  type ClientAction,
  type ClientView,
  type OwnDevice,
  type PairOffer,
  type ProfileFacts,
} from "./client";
import { Empty, SectionTitle, shortId } from "./kit";

type Dispatch = (action: ClientAction) => Promise<void>;

export type DeviceTone = "good" | "neutral" | "warn";

/**
 * The code to type on a device you already have — `null` when this one is not
 * waiting to be added.
 *
 * Whether a code exists is the daemon's answer and not a rule repeated here:
 * a device that already holds a profile shows none, one whose code was spent
 * shows none while its confirmation is outstanding, and both are the same
 * absence to a person. The addresses ride in the code itself because that is
 * how they are entered — one thing to read out, not two.
 */
export function codeToEnter(profile: ProfileFacts | null): string | null {
  if (profile === null || profile.pairing === null) return null;
  const { code, direct } = profile.pairing;
  return direct.length === 0 ? code : `${code}@${direct.join(",")}`;
}

/**
 * What was last learned about one device, in one chip.
 *
 * "Could not be reached" and "not checked yet" are deliberately different
 * lines: only one of them is worth acting on, and a device that could not be
 * asked has said nothing at all — least of all that it is off.
 */
export function deviceStanding(device: OwnDevice): { label: string; tone: DeviceTone } {
  if (device.liveness.kind === "answered") {
    return { label: device.me ? "This device" : "Reachable", tone: "good" };
  }
  if (device.liveness.kind === "couldNotAsk") return { label: "Could not be reached", tone: "warn" };
  return { label: device.me ? "This device" : "Not checked yet", tone: "neutral" };
}

/** The Spaces a device is listed in, counted rather than named. */
export function spacesHeld(device: OwnDevice): string {
  if (device.held.length === 0) return "No Spaces yet";
  return device.held.length === 1 ? "1 Space" : `${device.held.length} Spaces`;
}

/**
 * Why there are no device rows to draw, when there are none — and never
 * "you have no devices", which no person can be: a list that has not been
 * read is a question nobody answered, and saying it plainly is the whole
 * difference between waiting and being told something false.
 */
export function devicesAbsence(profile: ProfileFacts | null): string | null {
  if (profile === null) return "Reading your devices…";
  if (profile.deviceSetUnknown || profile.devices.length === 0) {
    return "This device has not read the list of your devices yet.";
  }
  return null;
}

/** "in 14 min", "in 2 h", or the plain fact that it is over. */
export function expiryLabel(expiresAtMs: number, now: number): string {
  const minutes = Math.round((expiresAtMs - now) / 60_000);
  if (minutes <= 0) return "expired";
  if (minutes < 60) return `in ${minutes} min`;
  return `in ${Math.round(minutes / 60)} h`;
}

/**
 * The code, as typed. Blank is not an act: a control that dispatched nothing
 * would spend a round trip to be told so.
 */
export function pairEnter(typed: string): ClientAction | null {
  const code = typed.trim();
  return code === "" ? null : { type: "devicePairEnter", code };
}

/** Whether Add device can be pressed: something typed, and not already asking. */
export function canAddDevice(view: ClientView, typed: string): boolean {
  return pairEnter(typed) !== null && !view.inFlight.includes(actionKey.devicePairEnter);
}

/**
 * Confirm and Reject answer one offer, so they share one key and disable
 * together — pressing one while the other is in flight would be two answers
 * to one question.
 */
export function answeringOffer(view: ClientView, pairing: string): boolean {
  return view.inFlight.includes(actionKey.devicePairConfirm(pairing));
}

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
    gridTemplateColumns: "auto minmax(0, 1fr)",
    alignItems: "center",
    columnGap: tokens.spacingHorizontalL,
    padding: `${tokens.spacingVerticalM} ${tokens.spacingHorizontalXL}`,
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
    backgroundColor: tokens.colorNeutralBackground1,
  },
  headerCopy: { display: "grid", gap: tokens.spacingVerticalXXS, minWidth: 0 },
  scroll: { overflowY: "auto", padding: `${tokens.spacingVerticalL} ${tokens.spacingHorizontalXL} ${tokens.spacingVerticalXXL}` },
  column: { maxWidth: "720px", margin: "0 auto", display: "grid", gap: tokens.spacingVerticalM },
  cards: { display: "grid", gap: tokens.spacingVerticalS },
  row: { display: "grid", gridTemplateColumns: "auto minmax(0, 1fr) auto", gap: tokens.spacingHorizontalM, alignItems: "center" },
  rowIcon: { color: tokens.colorNeutralForeground3, display: "grid", placeItems: "center", width: "28px" },
  rowCopy: { display: "grid", gap: tokens.spacingVerticalXXS, minWidth: 0 },
  rowTitle: { display: "flex", alignItems: "center", gap: tokens.spacingHorizontalS, flexWrap: "wrap" },
  muted: { color: tokens.colorNeutralForeground3 },
  crit: { color: tokens.colorPaletteRedForeground1 },
  chip: { whiteSpace: "nowrap", flexShrink: 0 },
  phrase: {
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase400,
    fontWeight: tokens.fontWeightSemibold,
    color: tokens.colorBrandForeground1,
    letterSpacing: "0.04em",
  },
  bigCode: {
    textAlign: "center",
    padding: `${tokens.spacingVerticalL} 0 ${tokens.spacingVerticalXS}`,
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeHero800,
    fontWeight: tokens.fontWeightSemibold,
    letterSpacing: "0.14em",
    userSelect: "all",
    wordBreak: "break-all",
  },
  waiting: { display: "grid", gap: tokens.spacingVerticalXS, textAlign: "center" },
  add: { display: "grid", gap: tokens.spacingVerticalS },
  addRow: { display: "grid", gridTemplateColumns: "minmax(0, 1fr) auto", gap: tokens.spacingHorizontalS, alignItems: "end" },
});

function Chip({ label, tone }: { label: string; tone: DeviceTone }) {
  const styles = useStyles();
  const color = tone === "good" ? "success" : tone === "warn" ? "warning" : "informative";
  return <FluentBadge appearance="tint" color={color} size="medium" shape="rounded" className={styles.chip}>{label}</FluentBadge>;
}

/** The clock, re-read every `everyMs`, so an expiry counts down without a view pump. */
function useNow(everyMs: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), everyMs);
    return () => clearInterval(timer);
  }, [everyMs]);
  return now;
}

export function DevicesSurface({ view, dispatch, onBack, ownedWindow = false }: {
  view: ClientView; dispatch: Dispatch; onBack(): void; ownedWindow?: boolean;
}) {
  const styles = useStyles();
  const profile = view.profile;
  const absence = devicesAbsence(profile);
  const waiting = codeToEnter(profile);
  return <section className={styles.surface} aria-label="Your devices">
    <header className={styles.header}>
      <Button appearance="subtle" icon={ownedWindow ? <Dismiss20Regular /> : <ArrowLeft20Regular />} onClick={onBack}>
        {ownedWindow ? "Close window" : "Library"}
      </Button>
      <div className={styles.headerCopy}>
        <Title3>Your devices</Title3>
        <Caption1 className={styles.muted}>Every device here is you. Adding one is done once — your Spaces follow on their own.</Caption1>
      </div>
    </header>
    <div className={styles.scroll}>
      <div className={styles.column}>
        {waiting !== null && profile?.pairing != null
          && <WaitingToBeAdded code={waiting} expiresAtMs={profile.pairing.expiresAtMs} />}
        {(profile?.offers ?? []).length > 0 && <section>
          <SectionTitle label="WAITING FOR YOU" count={profile?.offers.length} />
          <div className={styles.cards}>
            {profile?.offers.map((offer) => <PairOfferCard key={offer.pairing} offer={offer} view={view} dispatch={dispatch} />)}
          </div>
        </section>}
        <section>
          <SectionTitle label="YOUR DEVICES" count={profile?.devices.length} />
          {absence !== null
            ? <Empty said={absence} />
            : <div className={styles.cards}>
              {profile?.devices.map((device) => <DeviceRow key={device.device} device={device} />)}
            </div>}
        </section>
        {/* Offered even by a device that is itself waiting: two fresh
            machines both show a code, and whichever one a person is sitting
            at is the one that has to be able to type the other's. */}
        <AddDevice view={view} dispatch={dispatch} />
      </div>
    </div>
  </section>;
}

/**
 * This device, before it is anyone's. The code is the whole of what it can
 * offer — it has nothing to list and nobody to ask.
 */
function WaitingToBeAdded({ code, expiresAtMs }: { code: string; expiresAtMs: number }) {
  const styles = useStyles();
  const now = useNow(30_000);
  return <Card>
    <CardHeader header={<Text weight="semibold">This device is waiting to be added</Text>}
      description={<Caption1 className={styles.muted}>On a device you already use, open Your devices and type this code.</Caption1>} />
    <div className={styles.waiting}>
      <p className={styles.bigCode}>{code}</p>
      <Caption1 className={styles.muted}>The code stops working {expiryLabel(expiresAtMs, now)}. A new one appears when it does.</Caption1>
    </div>
  </Card>;
}

function DeviceRow({ device }: { device: OwnDevice }) {
  const styles = useStyles();
  const standing = deviceStanding(device);
  return <Card>
    <div className={styles.row}>
      <div className={styles.rowIcon}><Laptop20Regular /></div>
      <div className={styles.rowCopy}>
        <div className={styles.rowTitle}>
          <Text weight="semibold">{device.me ? "This device" : shortId(device.device)}</Text>
          <Chip {...standing} />
        </div>
        <Caption1 className={styles.muted}>{spacesHeld(device)}</Caption1>
        {device.liveness.kind === "couldNotAsk"
          && <Caption1 className={styles.muted}>It may be off or on another network — nothing it holds has been lost.</Caption1>}
      </div>
    </div>
  </Card>;
}

/**
 * The six words, and the two answers to them.
 *
 * The words are shown rather than checked: both devices derive them, and a
 * person comparing them is what tells a device of theirs from a stranger who
 * guessed at the same code. So they cross as the words, never as a flag
 * somebody else already decided.
 */
function PairOfferCard({ offer, view, dispatch }: { offer: PairOffer; view: ClientView; dispatch: Dispatch }) {
  const styles = useStyles();
  const busy = answeringOffer(view, offer.pairing);
  return <Card>
    <CardHeader header={<Text weight="semibold">{offer.name}</Text>}
      description={<Caption1 className={styles.muted}>Confirm only if this device shows these same six words.</Caption1>} />
    <p className={styles.phrase}>{offer.phrase.join("  ")}</p>
    <CardFooter>
      <Button appearance="primary" disabled={busy}
        onClick={() => void dispatch({ type: "devicePairConfirm", pairing: offer.pairing, accept: true })}>
        The words match
      </Button>
      <Button appearance="subtle" disabled={busy}
        onClick={() => void dispatch({ type: "devicePairConfirm", pairing: offer.pairing, accept: false })}>
        Not my device
      </Button>
    </CardFooter>
  </Card>;
}

/**
 * The one act. The field takes the code as it is shown on the other device,
 * including the addresses it may carry — which spellings mean the same code
 * is the daemon's rule, and a second one here could disagree with it.
 */
function AddDevice({ view, dispatch }: { view: ClientView; dispatch: Dispatch }) {
  const styles = useStyles();
  const [typed, setTyped] = useState("");
  const ready = canAddDevice(view, typed);
  const refused = view.failures.find((failure) => failure.key === actionKey.devicePairEnter);
  const add = () => {
    const action = pairEnter(typed);
    if (action === null) return;
    setTyped("");
    void dispatch(action);
  };
  return <Card>
    <CardHeader header={<Text weight="semibold">Add device</Text>}
      description={<Caption1 className={styles.muted}>The new device shows a code while it waits. Type it here, then compare the six words.</Caption1>} />
    <div className={styles.add}>
      <div className={styles.addRow}>
        <Field label="Code">
          <Input value={typed} placeholder="XXXX-XXXX" spellCheck={false}
            onChange={(_, data) => setTyped(data.value)}
            onKeyDown={(event) => { if (event.key === "Enter" && ready) add(); }} />
        </Field>
        <Button appearance="primary" disabled={!ready} onClick={add}>Add device</Button>
      </div>
      {refused !== undefined && <Body1 className={styles.crit}>{refused.error}</Body1>}
    </div>
  </Card>;
}
