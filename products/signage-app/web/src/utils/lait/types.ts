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
}

export type MediaSource =
  | { source: 'card'; title: string; body: string; background: string; foreground: string }
  | { source: 'stored'; content: string; size: number; mime: string }
  | { source: 'kind'; kind: string; settings: Record<string, string> }
  | { source: 'live'; resource: string };

export interface SignageMedia {
  id: string;
  name: string;
  source: MediaSource;
  duration_ms: number | null;
  width: number | null;
  height: number | null;
  /** Derived at ingest; present only for Stored entries that packaged. */
  catalog?: string | null;
}

export interface SignageConfig {
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

/** A window putting a different program on a screen. */
export interface ProgramWindow {
  id: string;
  window: ScheduleWindow;
  program: string;
}

export interface SignageScreen {
  id: string;
  name: string;
  group: string | null;
  intent: Slot;
  schedule: ProgramWindow[];
}

export interface SignageGroup {
  id: string;
  name: string;
  intent: Slot;
  screens: string[];
}

/** Which rung of override → schedule → direct → group answered. */
export type PlaybackSource = 'override' | 'schedule' | 'direct' | 'group';

export interface ProgramReply {
  kind: 'program';
  program: SignageProgram | null;
  /** The library entries the items name, in item order, deduplicated. */
  media: SignageMedia[];
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
/** The ladder's inputs, never its answer — the caller brings the clock. */
export interface PlaysReply {
  kind: 'plays';
  screen: SignageScreen | null;
  group?: SignageGroup | null;
}
export interface ScreenSavedReply {
  kind: 'screen_saved';
  screen: string;
}
export interface ScreenDeletedReply {
  kind: 'screen_deleted';
  screen: string;
}
export interface GroupReply {
  kind: 'group';
  group: SignageGroup | null;
}
export interface GroupsReply {
  kind: 'groups';
  groups: SignageGroup[];
}
export interface GroupSavedReply {
  kind: 'group_saved';
  group: string;
}
export interface GroupDeletedReply {
  kind: 'group_deleted';
  group: string;
}
export interface ConfigReply {
  kind: 'config';
  config: SignageConfig | null;
}
export interface ConfigsReply {
  kind: 'configs';
  configs: SignageConfig[];
}
export interface ConfigSavedReply {
  kind: 'config_saved';
  config: string;
}
export interface ConfigDeletedReply {
  kind: 'config_deleted';
  config: string;
}
