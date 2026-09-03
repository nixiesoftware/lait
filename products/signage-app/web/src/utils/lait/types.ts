/**
 * The Signage World's wire shapes, mirrored from the Rust contract
 * (products/signage/src/contract.rs, products/signage-app/src/protocol.rs).
 * Requests carry `cmd`, replies carry `kind`, both snake_case. These types
 * are the protocol — nothing here is a REST translation, and no field exists
 * that the contract does not state.
 */

export type ProgramCycle = 'hold_last' | 'loop' | 'poll_at_end' | 'blank_at_end';

export interface SignageItem {
  id: string;
  media: string;
  /** `null` takes the library entry's default — it never means "hold". */
  duration_ms: number | null;
}

export type Recurrence = 'none' | 'daily' | 'weekly' | 'monthly';

export interface ScheduleWindow {
  /** ISO-8601 civil datetime, no offset or zone annotation. */
  start_local: string;
  duration_ms: number;
  recurrence: Recurrence;
  until_unix_ms: number | null;
  priority: number;
  enabled: boolean;
  /** IANA identifier, e.g. `America/Chicago`. */
  timezone: string;
  exceptions?: unknown[];
}

/** A window choosing among this program's own items. */
export interface SignageWindow {
  id: string;
  window: ScheduleWindow;
  items: string[];
}

export interface SignageProgram {
  id: string;
  name: string;
  cycle: ProgramCycle;
  items: SignageItem[];
  windows: SignageWindow[];
  /**
   * Work in progress that is not on air. The editor autosaves here; a
   * screen never reads it; "Put on air" copies it over the fields above
   * and clears it. Absent on the wire when there is none.
   */
  draft?: ProgramDraft | null;
}

/** A program as it is being edited, minus its identity. */
export interface ProgramDraft {
  name: string;
  cycle: ProgramCycle;
  items: SignageItem[];
  windows: SignageWindow[];
}

/**
 * Flattened onto the media object on the wire (`#[serde(flatten)]`): the
 * discriminator is the `source` string and the variant's fields sit beside
 * `id` and `name`, not nested under a key. Card colors are bare 6-hex.
 */
export type MediaSource =
  | { source: 'card'; title: string; body: string; background: string; foreground: string }
  | { source: 'stored'; content: string; size: number; mime: string }
  | { source: 'kind'; kind: string; preset?: string | null; settings: Record<string, string> }
  | { source: 'live'; resource: string };

export interface MediaIdentity {
  id: string;
  name: string;
  duration_ms: number | null;
  width: number | null;
  height: number | null;
  /** Derived at ingest; present only for Stored entries that packaged. */
  catalog?: string | null;
}

export type SignageMedia = MediaIdentity & MediaSource;

/**
 * How a kind is presented. Named, reusable, and carrying nothing about where
 * it plays — which is what lets one preset serve every venue. There may be as
 * many per kind as you like; the old one-per-Space rule existed only to make a
 * lookup by kind unambiguous, and entries name their preset by id now.
 */
export interface SignagePreset {
  id: string;
  kind: string;
  name: string;
  settings: Record<string, string>;
}

export interface SlotChoice {
  member: string;
  chosen_unix_ms: number;
  chooser: string;
}

export interface SlotOverride {
  choice: SlotChoice;
  until_unix_ms: number;
}

/** The standing choice, and an override over it. */
export interface Slot {
  base?: SlotChoice;
  over?: SlotOverride;
}

/**
 * A window putting a different program on a screen. The schedule window is
 * flattened onto this object on the wire.
 */
export type ProgramWindow = { id: string; program: string } & ScheduleWindow;

/** A panel's geography. Typed because every location-aware kind wants it. */
export interface Place {
  latitude: number;
  longitude: number;
  /** IANA identifier. Required — a coordinate without one computes a
   *  plausible timetable for the wrong offset and nothing looks wrong. */
  timezone: string;
  region?: string | null;
}

/**
 * One panel.
 *
 * Facts are true of it; labels are what somebody decided to call it. A screen
 * is in one physical place because that is how places work, and in as many
 * labels as its owner finds useful because that is how thinking works.
 */
export interface SignageScreen {
  id: string;
  name: string;
  place?: Place | null;
  /** Per-kind venue facts: `athan` keeps its method, school, iqamah offsets. */
  facts?: Record<string, Record<string, string>>;
  /** A frame-lock cohort. One, because a panel locks to one wall. */
  sync?: string | null;
  labels?: string[];
  /** The channel it shows when nothing is being broadcast at it. */
  tuned?: string | null;
}

export interface ChannelWindow {
  id: string;
  program: string;
}
export type ChannelWindowOnWire = ChannelWindow & ScheduleWindow;

/** A standing stream a screen tunes to. Has its own dayparts. */
export interface SignageChannel {
  id: string;
  name: string;
  base?: string | null;
  schedule?: ChannelWindowOnWire[];
}

/** What an audience can ask about a place. */
export type PlaceMatch =
  | { kind: 'placed' }
  | { kind: 'region'; region: string }
  | { kind: 'timezone'; timezone: string }
  | { kind: 'within'; latitude: number; longitude: number; km: number };

export type Compare = 'is' | 'is_not' | 'above' | 'below';

/**
 * Who a transmission reaches — a predicate, never a membership list. Tagged
 * `match` because `fact` carries a `kind` of its own.
 */
export type Match =
  | { match: 'all' }
  | { match: 'screen'; screen: string }
  | { match: 'label'; label: string }
  | { match: 'place'; place: PlaceMatch }
  | { match: 'fact'; kind: string; key: string; value: string }
  | { match: 'tuned'; channel: string }
  | { match: 'observed'; key: string; compare?: Compare; value: string }
  | { match: 'audience'; audience: string }
  | { match: 'not'; of: Match }
  | { match: 'all_of'; of: Match[] }
  | { match: 'any_of'; of: Match[] };

export interface SignageAudience {
  id: string;
  name: string;
  rule: Match;
}

/** What a broadcast does to the screens it reaches. Open, not closed. */
export type BroadcastAction =
  | { action: 'play'; program: string }
  | { action: 'tune'; channel: string }
  | { action: 'blank' }
  | { action: 'restore' }
  | { action: 'kind'; kind: string; settings: Record<string, string> };

/** When it is open. A window is the common case; `when` is the same
 *  predicate language the audience is written in. */
export type Timing =
  | ({ timing: 'window' } & ScheduleWindow)
  | { timing: 'when'; of: Match; priority: number };

export interface SignageBroadcast {
  id: string;
  name: string;
  audience: string;
  action: BroadcastAction;
  timing: Timing;
  /** Broadcasts this one replaces — an all-clear travels faster than expiry. */
  supersedes?: string[];
  cancelled_at_unix_ms?: number | null;
}

/** What a screen says it actually played, signed by the screen. */
export interface AsRunEntry {
  program: string;
  item: string;
  started_unix_ms: number;
  ended_unix_ms: number;
  source?: string | null;
}
export interface SignageAsRun {
  id: string;
  screen: string;
  entries?: AsRunEntry[];
  observations?: Record<string, string>;
}

/**
 * What a screen is showing. Three states, not two: `blank` is a screen told to
 * go dark, `unaddressed` is a screen nothing reaches. Only one is a fault.
 */
export type Showing =
  | { showing: 'program'; program: string }
  | { showing: 'blank' }
  | { showing: 'unaddressed' }
  | { showing: 'kind'; kind: string; settings: Record<string, string> };

/** Which rung answered, by name — so "why is it showing that" is a sentence. */
export type Resolved =
  | { via: 'broadcast'; broadcast: string; name: string; audience: string; priority: number }
  | { via: 'channel'; channel: string; name: string; window?: string | null };

export interface Playback {
  showing: Showing;
  source?: Resolved | null;
  next_boundary_unix_ms?: number | null;
}

export interface ProgramReply {
  kind: 'program';
  program: SignageProgram | null;
  /** The library entries the items name, in item order, deduplicated. */
  media: SignageMedia[];
  /** Every kind presentation, joined so one round trip can resolve a preset. */
  presets?: SignagePreset[];
}
export interface ProgramsReply {
  kind: 'programs';
  programs: SignageProgram[];
}
export interface SavedReply {
  kind: 'saved';
  program: string;
}
export interface DeletedReply {
  kind: 'deleted';
  program: string;
}
export interface MediaReply {
  kind: 'media';
  media: SignageMedia | null;
}
export interface LibraryReply {
  kind: 'library';
  media: SignageMedia[];
}
export interface MediaSavedReply {
  kind: 'media_saved';
  media: string;
}
export interface MediaDeletedReply {
  kind: 'media_deleted';
  media: string;
}
export interface UsedByReply {
  kind: 'used_by';
  programs: string[];
}
export interface ScreenReply {
  kind: 'screen';
  screen: SignageScreen | null;
}
export interface ScreensReply {
  kind: 'screens';
  screens: SignageScreen[];
}
export interface ShowingReply {
  kind: 'showing';
  screens: string[];
}
/** Resolution's inputs, never its answer — the caller brings the clock. */
export interface PlaysReply {
  kind: 'plays';
  screen: SignageScreen | null;
  channels?: SignageChannel[];
  broadcasts?: SignageBroadcast[];
  audiences?: SignageAudience[];
  programs?: SignageProgram[];
  media?: SignageMedia[];
  presets?: SignagePreset[];
}
/** The blast radius, before anybody presses send. */
export interface ReachesReply {
  kind: 'reaches';
  screens: string[];
}
export interface ScreenSavedReply {
  kind: 'screen_saved';
  screen: string;
}
export interface ScreenDeletedReply {
  kind: 'screen_deleted';
  screen: string;
}
export interface ChannelReply {
  kind: 'channel';
  channel: SignageChannel | null;
}
export interface ChannelsReply {
  kind: 'channels';
  channels: SignageChannel[];
}
export interface ChannelSavedReply {
  kind: 'channel_saved';
  channel: string;
}
export interface ChannelDeletedReply {
  kind: 'channel_deleted';
  channel: string;
}
export interface AudienceReply {
  kind: 'audience';
  audience: SignageAudience | null;
}
export interface AudiencesReply {
  kind: 'audiences';
  audiences: SignageAudience[];
}
export interface AudienceSavedReply {
  kind: 'audience_saved';
  audience: string;
}
export interface AudienceDeletedReply {
  kind: 'audience_deleted';
  audience: string;
}
export interface BroadcastReply {
  kind: 'broadcast';
  broadcast: SignageBroadcast | null;
}
export interface BroadcastsReply {
  kind: 'broadcasts';
  broadcasts: SignageBroadcast[];
}
export interface BroadcastSavedReply {
  kind: 'broadcast_saved';
  broadcast: string;
}
export interface BroadcastDeletedReply {
  kind: 'broadcast_deleted';
  broadcast: string;
}
export interface AsRunReply {
  kind: 'as_run';
  asrun: SignageAsRun | null;
}
export interface PresetReply {
  kind: 'preset';
  preset: SignagePreset | null;
}
export interface PresetsReply {
  kind: 'presets';
  presets: SignagePreset[];
}
export interface PresetSavedReply {
  kind: 'preset_saved';
  preset: string;
}
export interface PresetDeletedReply {
  kind: 'preset_deleted';
  preset: string;
}
