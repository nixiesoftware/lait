/**
 * The television that shows a screen, as this World sees it.
 *
 * Signage has one word for hardware, and it is "screen". The host holds the
 * receivers and their trust; this World asks it, in its own words (`tv_*`),
 * which screen each one shows, and gets a code for a screen no TV shows yet.
 * A TV has no name here — the host's label for it is the screen's name.
 * Nothing here can see another World's TVs, and nothing here can take over
 * the machine's own screen — that stays with the linked-devices manager.
 */

import { useCallback, useEffect, useState } from 'react';

import { rpc } from '../api/client';

export interface TvHealth {
  connection: string;
  playback: string;
  last_error: string;
  reported_at_unix_ms: number | null;
}

export interface TvAssignment {
  assignment: string;
  surface: string;
  input: { screen?: string } & Record<string, unknown>;
  sync: { group: string; mode: string; static_delay_ms: number } | null;
  expires_at_unix_ms: number | null;
}

export interface TvReceiver {
  device: string;
  label: string;
  platform: string;
  build: string;
  issued_at_unix_ms: number;
  health: TvHealth | null;
  /** `null` is a TV nobody holds, which this World may point at a screen. */
  assignment: TvAssignment | null;
}

export interface TvCode {
  rendezvous: string;
  code: string;
  site: string | null;
  label: string;
  surface: string;
  input: { screen?: string } & Record<string, unknown>;
  state: 'waiting' | 'connecting' | 'connected';
  device: string | null;
  created_at_unix_ms: number;
  expires_at_unix_ms: number;
}

export interface TvPairing {
  pairing: string;
  confirmation_phrase: string[];
  platform: string;
  build: string;
  created_at_unix_ms: number;
  expires_at_unix_ms: number;
}

export interface TvFleet {
  site: string | null;
  receivers: TvReceiver[];
  codes: TvCode[];
  pairings: TvPairing[];
}

export const listTvs = () => rpc<TvFleet>({ cmd: 'tv_list' });
export const mintTvCode = (screen: string, label: string) =>
  rpc<{ rendezvous: string; code: string; site: string | null; expires_at_unix_ms: number }>({
    cmd: 'tv_code_mint',
    screen,
    label,
  });
export const revokeTvCode = (rendezvous: string) => rpc({ cmd: 'tv_code_revoke', rendezvous });
export const assignTv = (device: string, screen: string) => rpc({ cmd: 'tv_assign', device, screen });
export const forgetTv = (device: string) => rpc({ cmd: 'tv_forget', device });
export const approveTvPairing = (pairing: string, label: string, screen: string) =>
  rpc<{ device: string }>({ cmd: 'tv_pairing_approve', pairing, label, screen });
export const rejectTvPairing = (pairing: string) => rpc({ cmd: 'tv_pairing_reject', pairing });

/** The screen a receiver or code shows, or null. */
export function screenOf(input: { screen?: string } | null | undefined): string | null {
  return typeof input?.screen === 'string' ? input.screen : null;
}

/** What a person types on the television: the site, then the code. */
export function codeEntry(code: { site: string | null; code: string }): string {
  return code.site ? `${code.site}-${code.code}` : code.code;
}

/** `webos` → `webOS`, `android_tv` → `Android TV`, anything else title-cased. */
export function platformName(platform: string): string {
  const known: Record<string, string> = { webos: 'webOS', tizen: 'Tizen', android_tv: 'Android TV', fire_tv: 'Fire TV' };
  return known[platform] ?? platform.split('_').map((w) => w.charAt(0).toUpperCase() + w.slice(1)).join(' ');
}

/** A running receiver reports every 30–55 s; three minutes of silence is a TV that is gone. */
export const silentAfterMs = 3 * 60_000;
/** A fresh enrolment gets this long to report before its silence is called. */
export const connectingGraceMs = 2 * 60_000;

export type TvTone = 'good' | 'warn' | 'crit' | 'neutral';

/**
 * A TV's state in one word, by the clock as much as by the report: the
 * report cannot say "and then I stopped"; only the clock can.
 */
export function tvStatus(receiver: Pick<TvReceiver, 'health' | 'issued_at_unix_ms'>, now = Date.now()): { label: string; tone: TvTone } {
  if (receiver.health === null) {
    return now - receiver.issued_at_unix_ms < connectingGraceMs
      ? { label: 'Connecting…', tone: 'neutral' }
      : { label: 'Not heard from', tone: 'warn' };
  }
  const reported = receiver.health.reported_at_unix_ms;
  if (reported !== null && now - reported > silentAfterMs) {
    return { label: `Last seen ${agoLabel(now - reported)}`, tone: 'warn' };
  }
  switch (receiver.health.connection) {
    case 'online':
      return { label: 'Connected', tone: 'good' };
    case 'retrying':
      return { label: 'Reconnecting…', tone: 'warn' };
    case 'offline':
      return { label: 'Offline', tone: 'crit' };
    default:
      return { label: receiver.health.connection, tone: 'neutral' };
  }
}

export function agoLabel(ms: number): string {
  const minutes = Math.max(1, Math.round(ms / 60_000));
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours} h ago`;
  return `${Math.round(hours / 24)} days ago`;
}

/** Minutes a code has left, never below zero. */
export function minutesLeft(expiresAtUnixMs: number, now = Date.now()): number {
  return Math.max(0, Math.ceil((expiresAtUnixMs - now) / 60_000));
}

/**
 * The fleet as the host last answered, re-asked every `everyMs` while
 * mounted — TV state is the host's, not this World's, so it does not arrive
 * on the World's live feed.
 */
export function useTvs(everyMs = 5_000): { fleet: TvFleet | null; error: string | null; refresh: () => Promise<void> } {
  const [fleet, setFleet] = useState<TvFleet | null>(null);
  const [error, setError] = useState<string | null>(null);
  const refresh = useCallback(async () => {
    try {
      setFleet(await listTvs());
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, []);
  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), everyMs);
    return () => clearInterval(timer);
  }, [refresh, everyMs]);
  return { fleet, error, refresh };
}
