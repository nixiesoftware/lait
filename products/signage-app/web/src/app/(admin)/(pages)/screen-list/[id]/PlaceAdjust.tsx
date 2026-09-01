/**
 * The fine adjustments of a place: coordinates typed, the zone required,
 * the region named. The city search is deliberately not here — it belongs
 * to whatever input already owns the asking (the place combo's query, the
 * inspector's picker) — so this is only the part that comes after: nudging
 * what a pick resolved to, or entering a place no gazetteer lists.
 */

import { CommitSelect, CommitText } from "@/ds";
import type { Place, SignageScreen } from "@/utils/lait/types";

export function placeWith(screen: SignageScreen, patch: Partial<Place>): SignageScreen {
  const place = screen.place;
  return {
    ...screen,
    place: {
      latitude: place?.latitude ?? 0,
      longitude: place?.longitude ?? 0,
      timezone: place?.timezone ?? "",
      region: place?.region ?? null,
      ...patch,
    },
  };
}

export function PlaceAdjust({
  screen,
  put,
}: {
  screen: SignageScreen;
  put: (next: SignageScreen) => Promise<void>;
}) {
  const place = screen.place;
  const write = (patch: Partial<Place>) => put(placeWith(screen, patch));
  const zones =
    typeof Intl.supportedValuesOf === "function" ? Intl.supportedValuesOf("timeZone") : [];
  return (
    <>
      <div className="ds-pair">
        <CommitText
          label="Latitude"
          value={place ? String(place.latitude) : ""}
          placeholder="42.3314"
          inputMode="decimal"
          onWrite={(next) => write({ latitude: Number(next) })}
        />
        <CommitText
          label="Longitude"
          value={place ? String(place.longitude) : ""}
          placeholder="-83.0458"
          inputMode="decimal"
          onWrite={(next) => write({ longitude: Number(next) })}
        />
      </div>
      <div className="ds-pair">
        <CommitSelect
          label="Time zone"
          value={place?.timezone ?? ""}
          options={[
            { value: "", label: "Choose a zone" },
            ...zones.map((zone) => ({ value: zone, label: zone })),
          ]}
          onWrite={(next) => write({ timezone: next })}
        />
        <CommitText
          label="Region"
          value={place?.region ?? ""}
          placeholder="MI"
          onWrite={(next) => write({ region: next || null })}
        />
      </div>
    </>
  );
}
