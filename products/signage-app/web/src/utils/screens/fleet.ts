/**
 * The fleet and everything that reaches it, loaded once for a page.
 *
 * A bezel drawing what a screen shows needs the same seven lists a footprint
 * needs to say whom a rule reaches, which are the same seven the sky stack
 * needs to rank claims. One hook, one load, re-read on the doorbell, so the
 * three instruments on a page cannot disagree with each other.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { useRevision } from "@/ds";
import {
  fetchAudiences,
  fetchBroadcasts,
  fetchChannels,
  fetchPresets,
} from "@/utils/apps/api";
import { fetchPrograms } from "@/utils/broadcasts/api";
import { fetchLibrary } from "@/utils/content/api";
import { fetchScreens } from "@/utils/screens/api";
import {
  resolvePlayback,
  screensReached,
  type ResolutionInputs,
} from "@/utils/lait/resolve";
import type {
  Match,
  Playback,
  SignageAudience,
  SignageBroadcast,
  SignageChannel,
  SignageMedia,
  SignagePreset,
  SignageProgram,
  SignageScreen,
} from "@/utils/lait/types";

export type Fleet = {
  screens: SignageScreen[];
  channels: SignageChannel[];
  broadcasts: SignageBroadcast[];
  audiences: SignageAudience[];
  programs: SignageProgram[];
  media: SignageMedia[];
  presets: SignagePreset[];
  loading: boolean;
  error: string | null;
  reload: () => Promise<void>;
  /** What one screen shows at `now`, by the same ladder the World uses. */
  playbackFor: (screen: SignageScreen, now: number) => Playback;
  /** The ids a rule reaches, out of the whole fleet. */
  reachedBy: (rule: Match) => Set<string>;
  /** Screens tuned to a channel. */
  tunedTo: (channel: string) => SignageScreen[];
};

const EMPTY: Omit<Fleet, "loading" | "error" | "reload" | "playbackFor" | "reachedBy" | "tunedTo"> = {
  screens: [],
  channels: [],
  broadcasts: [],
  audiences: [],
  programs: [],
  media: [],
  presets: [],
};

export function useFleet(): Fleet {
  const revision = useRevision();
  const [held, setHeld] = useState(EMPTY);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      setError(null);
      const [screens, channels, broadcasts, audiences, programs, media, presets] =
        await Promise.all([
          fetchScreens(),
          fetchChannels(),
          fetchBroadcasts(),
          fetchAudiences(),
          fetchPrograms(),
          fetchLibrary(),
          fetchPresets(),
        ]);
      setHeld({ screens, channels, broadcasts, audiences, programs, media, presets });
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not load the fleet");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload, revision]);

  const playbackFor = useCallback(
    (screen: SignageScreen, now: number) => {
      const inputs: ResolutionInputs = { ...held, screen };
      return resolvePlayback(inputs, now);
    },
    [held],
  );

  const reachedBy = useCallback(
    (rule: Match) =>
      new Set(screensReached(rule, held.screens, held.audiences).map((screen) => screen.id)),
    [held],
  );

  const tunedTo = useCallback(
    (channel: string) => held.screens.filter((screen) => screen.tuned === channel),
    [held],
  );

  return useMemo(
    () => ({ ...held, loading, error, reload, playbackFor, reachedBy, tunedTo }),
    [held, loading, error, reload, playbackFor, reachedBy, tunedTo],
  );
}
