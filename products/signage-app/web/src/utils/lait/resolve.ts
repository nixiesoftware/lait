/**
 * The resolution ladder, in the browser.
 *
 * A port of `products/signage/src/fleet.rs` and `addressing.rs`. It exists so
 * an operator can be shown what a screen will do — which broadcast wins, what
 * an audience reaches, whether a panel is dark on purpose — without waiting for
 * the panel to tell them afterwards.
 *
 * Two rules it keeps because the Rust keeps them:
 *
 *   - **Absent is not zero.** An observation nobody reported fails every
 *     comparison rather than reading as a false one.
 *   - **Blank and unaddressed are different.** One is a screen told to go
 *     dark; the other is a screen nothing reaches. Only one is a fault.
 *
 * It does not schedule windows. `evaluate_at` lives in the `schedule` crate and
 * has no browser twin, so a `window` timing is treated as open and the caller
 * is told so — see `windowsAreAssumedOpen`. A preview that silently dropped
 * every scheduled broadcast would under-report a blast radius, which is the one
 * direction this must not be wrong in.
 */

import type {
  Match,
  Playback,
  Resolved,
  Showing,
  SignageAudience,
  SignageBroadcast,
  SignageChannel,
  SignageMedia,
  SignagePreset,
  SignageProgram,
  SignageScreen,
} from './types';

export type ResolutionInputs = {
  screen: SignageScreen | null;
  channels: SignageChannel[];
  broadcasts: SignageBroadcast[];
  audiences: SignageAudience[];
  programs: SignageProgram[];
  media: SignageMedia[];
  presets: SignagePreset[];
};

/** What the screen last reported about itself. Empty is not zero. */
export type Observations = Record<string, string>;

export type Context = { nowUnixMs: number; observations: Observations };

/**
 * Scheduled broadcasts are treated as open here, so a preview over-reports
 * rather than under-reports. Anything counting on exactness must ask the World.
 */
export const windowsAreAssumedOpen = true;

const MAX_HOPS = 8;

export function reaches(
  rule: Match,
  screen: SignageScreen,
  cx: Context,
  lookup: Map<string, Match>,
  hops = MAX_HOPS,
): boolean {
  switch (rule.match) {
    case 'all':
      return true;
    case 'screen':
      return screen.id === rule.screen;
    case 'label':
      return (screen.labels ?? []).includes(rule.label);
    case 'tuned':
      return (screen.tuned ?? null) === rule.channel;
    case 'place': {
      const place = screen.place;
      if (!place) return false;
      switch (rule.place.kind) {
        case 'placed':
          return true;
        case 'region':
          return (place.region ?? '').toLowerCase() === rule.place.region.toLowerCase();
        case 'timezone':
          return place.timezone === rule.place.timezone;
        case 'within':
          return (
            kmBetween(place.latitude, place.longitude, rule.place.latitude, rule.place.longitude) <=
            rule.place.km
          );
      }
      return false;
    }
    case 'fact':
      return screen.facts?.[rule.kind]?.[rule.key] === rule.value;
    case 'observed': {
      const observed = cx.observations[rule.key];
      if (observed === undefined) return false;
      return compare(observed, rule.compare ?? 'is', rule.value);
    }
    case 'audience': {
      if (hops <= 0) return false;
      const nested = lookup.get(rule.audience);
      return nested ? reaches(nested, screen, cx, lookup, hops - 1) : false;
    }
    case 'not':
      return !reaches(rule.of, screen, cx, lookup, hops);
    case 'all_of':
      return rule.of.every((term) => reaches(term, screen, cx, lookup, hops));
    case 'any_of':
      return rule.of.some((term) => reaches(term, screen, cx, lookup, hops));
  }
  return false;
}

function compare(observed: string, how: string, against: string): boolean {
  if (how === 'is') return observed === against;
  if (how === 'is_not') return observed !== against;
  const left = Number(observed);
  const right = Number(against);
  // Unparseable is absent, never zero.
  if (!Number.isFinite(left) || !Number.isFinite(right)) return false;
  return how === 'above' ? left > right : left < right;
}

function kmBetween(lat1: number, lon1: number, lat2: number, lon2: number): number {
  const earthKm = 6371;
  const toRad = (d: number) => (d * Math.PI) / 180;
  const dLat = toRad(lat2 - lat1);
  const dLon = toRad(lon2 - lon1);
  const a =
    Math.sin(dLat / 2) ** 2 +
    Math.cos(toRad(lat1)) * Math.cos(toRad(lat2)) * Math.sin(dLon / 2) ** 2;
  return 2 * earthKm * Math.asin(Math.min(1, Math.sqrt(a)));
}

function priorityOf(broadcast: SignageBroadcast): number {
  return broadcast.timing.timing === 'when'
    ? broadcast.timing.priority
    : (broadcast.timing.priority ?? 0);
}

/** Which screens an audience reaches, out of the ones given. */
export function screensReached(
  rule: Match,
  screens: SignageScreen[],
  audiences: SignageAudience[],
  cx: Context = { nowUnixMs: Date.now(), observations: {} },
): SignageScreen[] {
  const lookup = new Map(audiences.map((entry) => [entry.id, entry.rule]));
  return screens.filter((screen) => reaches(rule, screen, cx, lookup));
}

export function resolvePlayback(inputs: ResolutionInputs, nowUnixMs: number): Playback {
  const { screen, channels, broadcasts, audiences } = inputs;
  if (!screen) return { showing: { showing: 'unaddressed' } };

  const cx: Context = { nowUnixMs, observations: {} };
  const lookup = new Map(audiences.map((entry) => [entry.id, entry.rule]));

  const superseded = new Set(
    broadcasts
      .filter((entry) => !cancelled(entry, nowUnixMs))
      .flatMap((entry) => entry.supersedes ?? []),
  );

  let winner: SignageBroadcast | null = null;
  for (const broadcast of broadcasts) {
    if (cancelled(broadcast, nowUnixMs) || superseded.has(broadcast.id)) continue;
    const open =
      broadcast.timing.timing === 'when'
        ? reaches(broadcast.timing.of, screen, cx, lookup)
        : windowsAreAssumedOpen;
    if (!open) continue;
    const rule = lookup.get(broadcast.audience);
    if (!rule || !reaches(rule, screen, cx, lookup)) continue;
    if (
      !winner ||
      priorityOf(broadcast) > priorityOf(winner) ||
      (priorityOf(broadcast) === priorityOf(winner) && broadcast.id < winner.id)
    ) {
      winner = broadcast;
    }
  }

  if (winner) {
    const source: Resolved = {
      via: 'broadcast',
      broadcast: winner.id,
      name: winner.name,
      audience: winner.audience,
      priority: priorityOf(winner),
    };
    const action = winner.action;
    if (action.action === 'play') {
      return { showing: { showing: 'program', program: action.program }, source };
    }
    if (action.action === 'blank') return { showing: { showing: 'blank' }, source };
    if (action.action === 'kind') {
      return {
        showing: { showing: 'kind', kind: action.kind, settings: action.settings },
        source,
      };
    }
    if (action.action === 'tune') return fromChannel(action.channel, channels);
    // `restore` falls through to the channel, outranking anything below it.
  }

  return fromChannel(screen.tuned ?? null, channels);
}

function cancelled(broadcast: SignageBroadcast, nowUnixMs: number): boolean {
  const at = broadcast.cancelled_at_unix_ms;
  return at != null && nowUnixMs >= at;
}

function fromChannel(id: string | null, channels: SignageChannel[]): Playback {
  if (!id) return { showing: { showing: 'unaddressed' } };
  const channel = channels.find((entry) => entry.id === id);
  // Tuned to something that is not here. It shows the same as unaddressed, and
  // the absent source is how an operator finds out which it was.
  if (!channel) return { showing: { showing: 'unaddressed' } };

  const program = channel.base ?? channel.schedule?.[0]?.program ?? null;
  const showing: Showing = program
    ? { showing: 'program', program }
    : { showing: 'unaddressed' };
  return {
    showing,
    source: { via: 'channel', channel: channel.id, name: channel.name },
  };
}

/** "Why is this screen showing that", as a sentence somebody can act on. */
export function explain(playback: Playback, programName?: string): string {
  const what =
    playback.showing.showing === 'program'
      ? (programName ?? 'a program')
      : playback.showing.showing === 'blank'
        ? 'nothing, on purpose'
        : playback.showing.showing === 'kind'
          ? playback.showing.kind
          : 'nothing';
  const source = playback.source;
  if (!source) return `Showing ${what} — nothing is addressed to this screen.`;
  return source.via === 'broadcast'
    ? `Showing ${what} — the ${source.name} broadcast, priority ${source.priority}.`
    : `Showing ${what} — tuned to ${source.name}.`;
}
