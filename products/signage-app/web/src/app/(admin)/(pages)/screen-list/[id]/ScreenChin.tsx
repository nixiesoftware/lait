/**
 * The chin of the console, and the island it becomes.
 *
 * The screen and its controls are one object: a body wrapping the glass,
 * with a chin along the bottom holding one key per act. Pressing a key
 * swells the body's own bottom band upward over the glass — the foundation
 * row (title, the query's ground, the way out) anchors where the keys were,
 * and the structure above it arrives on the next beats: shape, body,
 * foundation, an arpeggio.
 *
 * The motion is a measured-height spring, not a keyframe: the hill's height
 * is always springing toward what its content asks — opening, closing,
 * unfolding More options, a result list growing under a query — and because
 * a spring carries velocity, any of it can be interrupted mid-flight and
 * simply retargeted. The open hill can also be flicked away: drag down and
 * release with intent.
 *
 * All four panels stay mounted behind the morph, so every query, fold and
 * draft is where it was left. The key wears the accent while its panel is
 * up: the key is the state of its own panel.
 */

import { useEffect, useRef, useState, type ReactNode } from "react";
import { MotionConfig, motion } from "motion/react";
import { CalendarDays, MapPin, Radio, Tv, X } from "lucide-react";
import { ComboSurface, haptic, useToast, type ChoiceItem } from "@/ds";
import { useCitySearch } from "@/program-editor/fields/CityPicker";
import { PlaceAdjust, placeWith } from "./PlaceAdjust";
import type { SignageScreen } from "@/utils/lait/types";

type ChinKey = "tune" | "place" | "broadcast" | "schedule";

/* One spring for every bound — subtle bounce, and it keeps its velocity
   when a new target arrives mid-flight. */
const SPRING = { type: "spring", duration: 0.56, bounce: 0.2 } as const;

const CLOSED_H = 60;

export function ScreenChin({
  screen,
  put,
  tuneItems,
  onTune,
  broadcast,
  schedule,
}: {
  screen: SignageScreen;
  put: (next: SignageScreen) => Promise<void>;
  tuneItems: ChoiceItem[];
  onTune: (id: string) => void;
  /** The Broadcast key's panel: what reaches this screen, drawn rich. */
  broadcast: ReactNode;
  /** The Schedule key's panel: this screen's day, drawn rich. */
  schedule: ReactNode;
}) {
  const [open, setOpen] = useState<ChinKey | null>(null);
  const [slot, setSlot] = useState<HTMLElement | null>(null);
  const [contentH, setContentH] = useState(0);
  const [placeQuery, setPlaceQuery] = useState("");
  const morph = useRef<HTMLDivElement>(null);
  const view = useRef<HTMLDivElement>(null);

  const press = (key: ChinKey) => setOpen(open === key ? null : key);

  // The content is measured, and the hill springs toward it — however the
  // content came to change: a panel swap, the fold, a list growing.
  useEffect(() => {
    const el = view.current;
    if (!el) return;
    const size = new ResizeObserver(() => setContentH(el.offsetHeight));
    size.observe(el);
    return () => size.disconnect();
  }, []);

  // Opening Place only turns the bar into a search: the surface blooms
  // when there is something to show — a query being typed, or a selected
  // location's adjustments — never for an empty panel.
  const expanded =
    open !== null && (open !== "place" || placeQuery.trim() !== "" || screen.place != null);

  // The view's measurement includes its own vertical breathing.
  const hillH = expanded ? Math.max(CLOSED_H, contentH) : CLOSED_H;

  // The shade mirrors the hill, so the cast shadow sits at the edge the
  // content actually reaches.
  useEffect(() => {
    morph.current?.parentElement?.style.setProperty("--chin-hill-h", `${hillH}px`);
  }, [hillH]);

  // The island collapses the way it appeared: a press outside it, or Escape.
  useEffect(() => {
    if (open === null) return;
    const away = (event: PointerEvent) => {
      if (morph.current && !morph.current.contains(event.target as Node)) setOpen(null);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(null);
    };
    document.addEventListener("pointerdown", away);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("pointerdown", away);
      document.removeEventListener("keydown", escape);
    };
  }, [open]);

  const titles: Record<ChinKey, string> = {
    tune: "Tune",
    place: "Place",
    broadcast: "What reaches it",
    schedule: "Today",
  };

  return (
    <MotionConfig reducedMotion="user">
      <div className={`ds-chin${open ? " is-open" : ""}`}>
        <motion.div
          ref={morph}
          className="ds-chin-morph"
          initial={false}
          animate={{ height: hillH }}
          transition={SPRING}
          drag={open ? "y" : false}
          dragConstraints={{ top: 0, bottom: 0 }}
          dragElastic={{ top: 0, bottom: 0.55 }}
          onDragEnd={(_, info) => {
            // A flick is enough; a slow drag has to mean it.
            if (info.velocity.y > 450 || info.offset.y > 110) setOpen(null);
          }}
        >
          <motion.div
            ref={view}
            className="ds-chin-view"
            inert={open === null ? true : undefined}
            initial={false}
            animate={
              open
                ? {
                    opacity: 1,
                    y: 0,
                    filter: "blur(0px)",
                    transition: { delay: 0.06, duration: 0.26 },
                  }
                : {
                    opacity: 0,
                    y: 14,
                    filter: "blur(8px)",
                    transition: { duration: 0.14 },
                  }
            }
          >
            <div hidden={open !== "tune"}>
              <ComboSurface
                label="Tune"
                placeholder="Filter channels…"
                items={tuneItems}
                onPick={(id) => {
                  onTune(id);
                  setOpen(null);
                }}
                empty="No channel by that name."
                active={open === "tune"}
                inverted
                findSlot={open === "tune" ? slot : null}
              />
            </div>
            <div hidden={open !== "place"}>
              <PlaceSurface
                screen={screen}
                put={put}
                active={open === "place"}
                findSlot={open === "place" ? slot : null}
                query={placeQuery}
                onQueryChange={setPlaceQuery}
              />
            </div>
            <div hidden={open !== "broadcast"}>{broadcast}</div>
            <div hidden={open !== "schedule"}>{schedule}</div>
          </motion.div>
        </motion.div>
        <div className="ds-chin-shade" aria-hidden />
        {/* The foundation: the flat chin the hill rises from behind. Keys at
            rest; the anchored constants when a panel is up, landing last. */}
        <div className="ds-chin-band">
          <motion.div
            className="ds-chin-keys"
            role="group"
            aria-label="Screen actions"
            inert={open !== null ? true : undefined}
            initial={false}
            animate={
              open
                ? { opacity: 0, transition: { duration: 0.12 } }
                : { opacity: 1, transition: { delay: 0.05, duration: 0.2 } }
            }
          >
            <ChinButton label="Tune" open={open === "tune"} onPress={() => press("tune")}>
              <Tv size={16} aria-hidden />
            </ChinButton>
            <ChinButton label="Place" open={open === "place"} onPress={() => press("place")}>
              <MapPin size={16} aria-hidden />
            </ChinButton>
            <ChinButton
              label="Broadcast"
              open={open === "broadcast"}
              onPress={() => press("broadcast")}
            >
              <Radio size={16} aria-hidden />
            </ChinButton>
            <ChinButton
              label="Schedule"
              open={open === "schedule"}
              onPress={() => press("schedule")}
            >
              <CalendarDays size={16} aria-hidden />
            </ChinButton>
          </motion.div>
          <motion.div
            className="ds-chin-anchor"
            inert={open === null ? true : undefined}
            initial={false}
            animate={
              open
                ? { opacity: 1, y: 0, transition: { delay: 0.13, duration: 0.24 } }
                : { opacity: 0, y: 8, transition: { duration: 0.12 } }
            }
          >
            <strong>{open ? titles[open] : ""}</strong>
            <span className="ds-chin-anchor-slot" ref={setSlot} />
            <button
              type="button"
              className="ds-icon"
              aria-label="Put away"
              onClick={() => setOpen(null)}
            >
              <X size={15} />
            </button>
          </motion.div>
        </div>
      </div>
    </MotionConfig>
  );
}

function ChinButton({
  label,
  open,
  onPress,
  children,
}: {
  label: string;
  open: boolean;
  onPress: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      className="ds-railbtn"
      aria-expanded={open}
      data-popup-open={open || undefined}
      onClick={onPress}
    >
      <span className="ds-railbtn-key">{children}</span>
      {label}
    </button>
  );
}

/**
 * The place, asked for the way a person knows it: by city. The query is the
 * gazetteer ask, the pick is the commit — coordinates, zone and region land
 * in one act — and behind the fold sit the adjustments for nudging what the
 * pick resolved to, or entering a place no gazetteer lists.
 */
function PlaceSurface({
  screen,
  put,
  active,
  findSlot,
  query,
  onQueryChange,
}: {
  screen: SignageScreen;
  put: (next: SignageScreen) => Promise<void>;
  active: boolean;
  findSlot: HTMLElement | null;
  query: string;
  onQueryChange: (query: string) => void;
}) {
  const toast = useToast();
  const search = useCitySearch(query);

  const items: ChoiceItem[] = search.results
    .filter((city) => city.timezone)
    .map((city) => ({
      id: String(city.id),
      label: city.name,
      hint: `${[city.admin1, city.country].filter(Boolean).join(", ")} · ${city.timezone}`,
    }));

  const pick = (id: string) => {
    const city = search.results.find((entry) => String(entry.id) === id);
    if (!city?.timezone) return;
    void put(
      placeWith(screen, {
        latitude: city.latitude,
        longitude: city.longitude,
        timezone: city.timezone,
        region: city.admin1 ?? screen.place?.region ?? null,
      }),
    )
      .then(() => haptic("save"))
      .catch((err) => {
        haptic("error");
        toast.show("Could not place the screen", err instanceof Error ? err.message : String(err));
      });
  };

  return (
    <ComboSurface
      label="Place"
      placeholder="Search a city…"
      query={query}
      onQueryChange={onQueryChange}
      items={items}
      onPick={pick}
      status={search.loading ? "Searching…" : search.error}
      statusTone={search.error ? "danger" : "quiet"}
      empty={query.trim().length < 2 ? null : "No city by that name."}
      active={active}
      inverted
      findSlot={findSlot}
    >
      {/* The adjustments exist only once a location does: nothing to nudge
          before a pick, so no fold to promise it. */}
      {screen.place ? <PlaceAdjust screen={screen} put={put} /> : null}
    </ComboSurface>
  );
}
