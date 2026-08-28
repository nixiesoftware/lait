/**
 * A screen, drawn as a screen.
 *
 * The same object at three sizes — a row, a tile, the hero of its own page —
 * showing what it shows: the frame that is on it now, the Athan card with live
 * times, dark on purpose, or the empty horizon nothing reaches. The state that
 * matters is drawn on the object, never captioned beside it: on air is a rim of
 * alarm, never heard from is a rim of ochre.
 *
 * It is the one dark object on a light page, because a display panel is dark
 * in every room.
 */

import { useMemo, type ReactNode } from "react";
import { Thumb } from "@/program-editor/Thumb";
import { panelFor } from "@/program-editor/kinds/registry";
import { itemAtTime, layout, mediaById } from "@/program-editor/model";
import type {
  Playback,
  SignageMedia,
  SignagePreset,
  SignageProgram,
  SignageScreen,
} from "@/utils/lait/types";

export type BezelSize = "xs" | "sm" | "md" | "lg";

/** How a panel last spoke. `null` is not zero — it is "never". */
export type Heard = { at: number | null };

export type BezelProps = {
  size?: BezelSize;
  screen: SignageScreen;
  playback: Playback | null;
  programs: SignageProgram[];
  media: SignageMedia[];
  presets: SignagePreset[];
  orbit: string | null;
  /** The shared tick. A hundred bezels each with a clock disagree by a frame. */
  now: number;
  heard?: Heard;
  /** Something to draw over the glass — a selection box, a more menu. */
  children?: ReactNode;
  className?: string;
};

/**
 * Where a looping program is at `now`. The panel's real phase is its own; this
 * is the same loop, started at the epoch, which is enough for a preview that
 * changes on the tick rather than sitting on the first frame forever.
 */
function currentClip(
  program: SignageProgram,
  library: SignageMedia[],
  now: number,
) {
  const clips = layout(program, mediaById(library));
  if (clips.length === 0) return null;
  const total = clips.reduce((sum, clip) => sum + clip.durationMs, 0);
  if (total <= 0) return clips[0];
  return itemAtTime(clips, now % total);
}

/** A venue's coordinates, as the strings a kind reads. Empty when unplaced. */
function placeSettings(screen: SignageScreen): Record<string, string> {
  return screen.place
    ? {
        latitude: String(screen.place.latitude),
        longitude: String(screen.place.longitude),
        timezone: screen.place.timezone,
      }
    : {};
}

/** The settings a kind draws from: preset, then the entry, then the venue. */
function kindSettings(
  media: SignageMedia,
  screen: SignageScreen,
  presets: SignagePreset[],
): Record<string, string> {
  if (media.source !== "kind") return {};
  const preset = media.preset ? presets.find((entry) => entry.id === media.preset) : null;
  return {
    ...(preset?.settings ?? {}),
    ...media.settings,
    ...placeSettings(screen),
    ...(screen.facts?.[media.kind] ?? {}),
  };
}

export function Bezel({
  size = "sm",
  screen,
  playback,
  programs,
  media,
  presets,
  orbit,
  now,
  heard,
  children,
  className,
}: BezelProps) {
  const showing = playback?.showing ?? { showing: "unaddressed" as const };
  const onAir = playback?.source?.via === "broadcast";
  const never = heard != null && heard.at == null;

  const glass = useMemo(() => {
    if (showing.showing === "program") {
      const program = programs.find((entry) => entry.id === showing.program);
      const clip = program ? currentClip(program, media, now) : null;
      if (!clip?.media) return <span className="ds-glass is-empty" />;
      const entry = clip.media;
      if (entry.source === "kind") {
        const panel = panelFor(entry.kind);
        if (panel) {
          return (
            <span className="ds-glass is-kind">
              <panel.Preview
                settings={kindSettings(entry, screen, presets)}
                density={size === "lg" ? "stage" : size === "md" ? "panel" : "thumb"}
              />
            </span>
          );
        }
      }
      return (
        <span className="ds-glass">
          <Thumb media={entry} orbit={orbit} />
        </span>
      );
    }
    if (showing.showing === "kind") {
      const panel = panelFor(showing.kind);
      if (panel) {
        return (
          <span className="ds-glass is-kind">
            <panel.Preview
              settings={{
                ...showing.settings,
                ...placeSettings(screen),
                ...(screen.facts?.[showing.kind] ?? {}),
              }}
              density={size === "lg" ? "stage" : size === "md" ? "panel" : "thumb"}
            />
          </span>
        );
      }
    }
    if (showing.showing === "blank") return <span className="ds-glass is-dark" />;
    return <span className="ds-glass is-horizon" />;
    // The dependency on the tick is deliberate: a looping program advances.
  }, [showing, programs, media, presets, orbit, screen, now, size]);

  const state =
    showing.showing === "unaddressed"
      ? "unaddressed"
      : showing.showing === "blank"
        ? "blank"
        : "showing";

  return (
    <span
      className={`ds-bezel is-${size}${className ? ` ${className}` : ""}`}
      data-state={state}
      data-onair={onAir || undefined}
      data-never={never || undefined}
    >
      {glass}
      {children}
    </span>
  );
}
