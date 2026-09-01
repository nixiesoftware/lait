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
import { useEffect, useRef, useState, type RefObject } from "react";

import {
  actionKey,
  type ClientAction,
  type ClientView,
  type Marker,
  type OwnDevice,
  type PairOffer,
  type ProfileFacts,
  type SpaceRow,
} from "./client";
import { Button, Card, Chip, Empty, Field, PaneHead, SectionTitle, shortId } from "./kit";
import { IconCheckCircle, IconLaptop } from "./icons";

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

/**
 * A marker, as a person reads it: where it answers, without the scheme.
 *
 * Not a friendly name invented here. Whatever a marker is called is its own
 * to say, and a client that renamed one would be putting a word a person
 * trusts in front of a service that never claimed it.
 */
export function markerName(marker: string): string {
  return marker.replace(/^https?:\/\//, "").replace(/\/+$/, "");
}

/**
 * What one marker's last check found, on the list of markers.
 *
 * "Nothing has asked yet" and "it could not be reached" are two different
 * lines and neither is a warning: a marker being down is not a finding about
 * a person's devices, and drawing it as one would make an outage look like a
 * verdict. The four below it are findings — about the marker.
 */
export function markerStanding(marker: Marker): { label: string; tone: DeviceTone; said: string } {
  switch (marker.standing.kind) {
    case "answering":
      return { label: "Answering", tone: "good", said: "It answered, and what it said checks out." };
    case "neverAsked":
      return { label: "Not checked yet", tone: "neutral", said: "This device has not looked at it yet." };
    case "couldNotAsk":
      return {
        label: "Could not be reached",
        tone: "neutral",
        said: "It may be down or on another network. Anything it listed before still stands.",
      };
    case "answeredAsAnother":
      return {
        label: "Answered as something else",
        tone: "warn",
        said: "Something other than the marker this device follows is answering here. Nothing it says is counted.",
      };
    case "answeredOlder":
      return {
        label: "Answered with less than before",
        tone: "warn",
        said: "It answered with less than it had already recorded. What it proved earlier still stands; this answer does not.",
      };
    case "unproven":
      return {
        label: "Could not show its record",
        tone: "warn",
        said: "It could not show this answer carries forward what this device already had. What it proved earlier still stands.",
      };
    case "contradicted":
      return {
        label: "Contradicted itself",
        tone: "warn",
        said: "It gave two records that cannot both be true. Nothing it says is counted.",
      };
    case "unreadable":
      return {
        label: "Answer could not be read",
        tone: "warn",
        said: "Its answer was not something this device could read. What it proved earlier still stands.",
      };
  }
}

/**
 * What each marker says about one device — a tier, never a gate.
 *
 * Four lines, and the difference between them is the whole point. Being
 * listed is the only one that adds anything; the other three take nothing
 * away, and the two that are not answers ("not checked yet", "could not be
 * reached") must never be drawn as the one that is ("not listed"), because a
 * marker that is down has said nothing at all about anybody.
 *
 * Nothing on this surface reads the result: no control is disabled, no row is
 * hidden, and no act is refused because a marker is silent.
 */
export function certification(
  device: OwnDevice,
  markers: Marker[],
): { marker: string; label: string; tone: DeviceTone }[] {
  return markers.map((marker) => {
    const name = markerName(marker.marker);
    if (device.certifiedBy.includes(marker.marker)) {
      return { marker: marker.marker, label: `Listed by ${name}`, tone: "good" as const };
    }
    if (marker.standing.kind === "answering") {
      return { marker: marker.marker, label: `Not listed by ${name}`, tone: "neutral" as const };
    }
    if (marker.standing.kind === "neverAsked") {
      return { marker: marker.marker, label: `${name} not checked yet`, tone: "neutral" as const };
    }
    return { marker: marker.marker, label: `${name} could not be checked`, tone: "neutral" as const };
  });
}

/** The Spaces a device is listed in, counted rather than named. */
export function spacesHeld(device: OwnDevice): string {
  if (device.held.length === 0) return "No Spaces yet";
  return device.held.length === 1 ? "1 Space" : `${device.held.length} Spaces`;
}

/**
 * Where one Space stands towards one device, as a chip.
 *
 * Seven answers, and keeping them apart is the whole point of drawing them
 * at all. "Not on this device" is something a person chose; "could not be
 * reached" is a network; "not offered yet" is a loop that has not got there.
 * Only the first is worth acting on, and folding the other two into it would
 * put words in a person's mouth.
 */
export function spaceStanding(
  space: SpaceRow,
  device: string,
): { label: string; tone: DeviceTone; said: string } {
  const standing = space.standings.find((row) => row.device === device)?.standing;
  if (standing === undefined) {
    return space.on.includes(device)
      ? { label: "Held", tone: "good", said: "This Space is on that device." }
      : { label: "Not offered yet", tone: "neutral", said: "Nothing has offered it to that device yet." };
  }
  switch (standing.kind) {
    case "held":
      return { label: "Held", tone: "good", said: "This Space is on that device." };
    case "excluded":
      return standing.told
        ? { label: "Not on this device", tone: "neutral", said: "You took it off that device. Nothing on it was deleted." }
        : {
          label: "Coming off this device",
          tone: "neutral",
          said: "That device has not heard yet — it will as soon as it can be reached.",
        };
    case "couldNotAsk":
      return { label: "Could not be reached", tone: "warn", said: `That device did not answer: ${standing.why}` };
    case "declined":
      return { label: "The device said no", tone: "neutral", said: standing.why };
    case "refused":
      return { label: "Refused", tone: "warn", said: standing.why };
    case "deferred":
      return { label: "Not yet", tone: "neutral", said: standing.why };
  }
}

/**
 * Whether the person is asking for this Space to come off that device, or to
 * go back on it. `null` for the device you are sitting at, which holds what
 * it holds — taking a Space off *here* is leaving it, and that is the
 * Library's act rather than this one's.
 */
export function excludeAction(
  space: SpaceRow,
  device: OwnDevice,
): Extract<ClientAction, { type: "replicaExclude" }> | null {
  if (device.me) return null;
  const standing = space.standings.find((row) => row.device === device.device)?.standing;
  const excluded = standing?.kind === "excluded";
  return { type: "replicaExclude", device: device.device, space: space.space, excluded: !excluded };
}

/** Disabled on the frame it is pressed; both answers share one key. */
export function excluding(view: ClientView, space: string, device: string): boolean {
  return view.inFlight.includes(actionKey.replicaExclude(space, device));
}

/**
 * What retiring one device costs, said before it happens.
 *
 * Named rather than counted away: the Spaces it holds stop listing it, and
 * — the part a person cannot undo by clicking again — anything already
 * sealed to it stays readable there until an admin rotates that Space's key.
 * Nothing on the machine is deleted, and saying so is half the sentence: the
 * fear this control raises is "have I just wiped my laptop", and the answer
 * is no.
 */
export function retireWarning(device: OwnDevice): string {
  const spaces = device.held.length === 1 ? "1 Space" : `${device.held.length} Spaces`;
  // Which Spaces, exactly: the ones it is on your actor's list in. A Space it
  // entered as somebody else is not this act's to touch, and saying
  // "everywhere" would promise more than happens.
  const listed = device.held.length === 0
    ? "It is not listed in any Space of yours."
    : `It stops being listed in ${spaces} you share an actor in.`;
  return `${listed} Nothing on it is deleted, and it is not asked first — it may be off. `
    + "Anything already shared with it stays readable there until a Space key is rotated.";
}

/**
 * Whether this device can be retired from here. Never this one: the machine
 * you are sitting at cannot sign away its own place in the profile, and the
 * daemon refuses it too — a control that offered it would be offering a
 * refusal.
 */
export function canRetire(view: ClientView, device: OwnDevice): boolean {
  return !device.me && !view.inFlight.includes(actionKey.deviceRetire(device.device));
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

/** The clock, re-read every `everyMs`, so an expiry counts down without a view pump. */
function useNow(everyMs: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), everyMs);
    return () => clearInterval(timer);
  }, [everyMs]);
  return now;
}

export function DevicesPane({ view, dispatch }: { view: ClientView; dispatch: Dispatch }) {
  const profile = view.profile;
  const absence = devicesAbsence(profile);
  const waiting = codeToEnter(profile);
  const codeField = useRef<HTMLInputElement>(null);
  return <div className="secondary-scroll" aria-label="Your devices">
      <div className="content-column narrow">
        <PaneHead title="Your devices"
          action={<Button variant="primary" onPress={() => {
            codeField.current?.scrollIntoView({ behavior: "smooth", block: "center" });
            codeField.current?.focus({ preventScroll: true });
          }}>Add device</Button>} />
        {waiting !== null && profile?.pairing != null
          && <WaitingToBeAdded code={waiting} expiresAtMs={profile.pairing.expiresAtMs} />}
        {(profile?.offers ?? []).length > 0 && <section>
          <SectionTitle label="Waiting for you" count={profile?.offers.length} />
          <div className="card-stack">
            {profile?.offers.map((offer) => <PairOfferCard key={offer.pairing} offer={offer} view={view} dispatch={dispatch} />)}
          </div>
        </section>}
        <section>
          <SectionTitle label="Your devices" count={profile?.devices.length} />
          {absence !== null
            ? <Empty said={absence} />
            : <div className="card-stack">
              {profile?.devices.map((device) =>
                <DeviceRow key={device.device} device={device} markers={profile.markers}
                  spaces={profile.spaces} view={view} dispatch={dispatch} />)}
            </div>}
        </section>
        {/* Only when this person weighs any. A device with no markers in its
            book has nothing to draw here, and an empty section would invite
            the reading that something is missing. */}
        {(profile?.markers ?? []).length > 0 && <section>
          <SectionTitle label="Who lists them" count={profile?.markers.length} />
          <div className="card-stack">
            {profile?.markers.map((marker) => <MarkerRow key={marker.marker} marker={marker} />)}
          </div>
        </section>}
        {/* Offered even by a device that is itself waiting: two fresh
            machines both show a code, and whichever one a person is sitting
            at is the one that has to be able to type the other's. */}
        <AddDevice view={view} dispatch={dispatch} codeField={codeField} />
      </div>
  </div>;
}

/**
 * This device, before it is anyone's. The code is the whole of what it can
 * offer — it has nothing to list and nobody to ask.
 */
function WaitingToBeAdded({ code, expiresAtMs }: { code: string; expiresAtMs: number }) {
  const now = useNow(30_000);
  return <Card>
    <div className="item-copy">
      <div className="item-title"><strong>This device is waiting to be added</strong></div>
      <small className="dim-line">On a device you already use, open Your devices and type this code.</small>
    </div>
    <p className="big-code">{code}</p>
    <small className="dim-line centered">The code stops working {expiryLabel(expiresAtMs, now)}. A new one appears when it does.</small>
  </Card>;
}

function DeviceRow({ device, markers, spaces, view, dispatch }: {
  device: OwnDevice; markers: Marker[]; spaces: SpaceRow[]; view: ClientView; dispatch: Dispatch;
}) {
  const [asking, setAsking] = useState(false);
  const standing = deviceStanding(device);
  // Drawn beside the device, never in front of it: this row is complete
  // whether or not anybody has ever listed the device, and the chips add to
  // it rather than qualify it.
  const listings = certification(device, markers);
  return <Card>
    <div className="item-row">
      <div className="item-icon"><IconLaptop /></div>
      <div className="item-copy">
        <div className="item-title">
          <strong>{device.me ? "This device" : shortId(device.device)}</strong>
          <Chip label={standing.label} tone={standing.tone} />
        </div>
        <small className="dim-line">{spacesHeld(device)}</small>
        {device.liveness.kind === "couldNotAsk"
          && <small className="dim-line">It may be off or on another network — nothing it holds has been lost.</small>}
        {listings.length > 0 && <div className="item-title">
          {listings.map((listing) => <Chip key={listing.marker} label={listing.label} tone={listing.tone} />)}
        </div>}
        {asking && <small className="dim-line">{retireWarning(device)}</small>}
        {!device.me && spaces.map((space) => {
          const spaceState = spaceStanding(space, device.device);
          const act = excludeAction(space, device);
          return <div key={space.space} className="item-title">
            <small className="dim-line">{shortId(space.space)}</small>
            <Chip label={spaceState.label} tone={spaceState.tone} />
            {act !== null && <Button variant="ghost"
              disabled={excluding(view, space.space, device.device)}
              onPress={() => void dispatch(act)}>
              {act.excluded ? "Not on this device" : "Put back"}
            </Button>}
          </div>;
        })}
      </div>
      {/* Asked twice on purpose. Retiring is reversible only by pairing the
          machine again, and the sentence it costs is worth reading. */}
      {!device.me && (asking
        ? <div className="button-row">
          <Button variant="primary" disabled={!canRetire(view, device)}
            onPress={() => { setAsking(false); void dispatch({ type: "deviceRetire", device: device.device }); }}>
            Retire it
          </Button>
          <Button variant="ghost" onPress={() => setAsking(false)}>Keep it</Button>
        </div>
        : <Button variant="ghost" onPress={() => setAsking(true)}>Retire</Button>)}
    </div>
  </Card>;
}

/**
 * One marker and how the last look at it went.
 *
 * Its own section rather than a footnote on every device, because every
 * sentence here is about the marker — a marker that could not be reached is
 * a fact about that service, and repeating it under each device would read
 * as a finding about the devices.
 */
function MarkerRow({ marker }: { marker: Marker }) {
  const standing = markerStanding(marker);
  return <Card>
    <div className="item-row">
      <div className="item-icon"><IconCheckCircle /></div>
      <div className="item-copy">
        <div className="item-title">
          <strong>{markerName(marker.marker)}</strong>
          <Chip label={standing.label} tone={standing.tone} />
        </div>
        <small className="dim-line">{standing.said}</small>
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
  const busy = answeringOffer(view, offer.pairing);
  return <Card>
    <div className="item-copy">
      <div className="item-title"><strong>{offer.name}</strong></div>
      <small className="dim-line">Confirm only if this device shows these same six words.</small>
    </div>
    <p className="phrase">{offer.phrase.join("  ")}</p>
    <div className="button-row">
      <Button variant="primary" disabled={busy}
        onPress={() => void dispatch({ type: "devicePairConfirm", pairing: offer.pairing, accept: true })}>
        The words match
      </Button>
      <Button variant="ghost" disabled={busy}
        onPress={() => void dispatch({ type: "devicePairConfirm", pairing: offer.pairing, accept: false })}>
        Not my device
      </Button>
    </div>
  </Card>;
}

/**
 * The one act. The field takes the code as it is shown on the other device,
 * including the addresses it may carry — which spellings mean the same code
 * is the daemon's rule, and a second one here could disagree with it.
 */
function AddDevice({ view, dispatch, codeField }: {
  view: ClientView; dispatch: Dispatch; codeField: RefObject<HTMLInputElement | null>;
}) {
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
    <div className="item-copy">
      <div className="item-title"><strong>Add device</strong></div>
      <small className="dim-line">The new device shows a code while it waits. Type it here, then compare the six words.</small>
    </div>
    <div className="field-row">
      <Field label="Code">
        <input ref={codeField} value={typed} placeholder="XXXX-XXXX" spellCheck={false}
          onChange={(event) => setTyped(event.target.value)}
          onKeyDown={(event) => { if (event.key === "Enter" && ready) add(); }} />
      </Field>
      <Button variant="primary" disabled={!ready} onPress={add}>Add device</Button>
    </div>
    {refused !== undefined && <p className="body-line danger-text">{refused.error}</p>}
  </Card>;
}
