import React, { useEffect, useMemo, useRef, useState } from "react";

// CitySelection is what we hand back to the parent on a successful pick.
// We expose every field the form might want to fill: callers commonly
// use latitude/longitude, plus timezone and a display city label.
export interface CitySelection {
  name: string;        // "Detroit"
  admin1?: string;     // "Michigan" (state/province; not always present)
  country: string;     // "United States"
  latitude: number;
  longitude: number;
  timezone: string;    // IANA — Open-Meteo always returns this
}

interface CityPickerProps {
  // Optional saved value the user is editing — we don't reverse-geocode
  // (Open-Meteo doesn't expose a reverse endpoint), so we just show
  // the stored coords as a hint until the user picks a new city.
  currentLatitude?: number | null;
  currentLongitude?: number | null;
  // Optional pre-filled label, used when the parent has a previously
  // saved "city" label and the user hasn't picked anything new yet.
  initialLabel?: string | null;
  onSelect: (s: CitySelection) => void;
}

// Open-Meteo's free geocoding endpoint. No API key, no documented rate
// limit for personal use, returns IANA timezone in every result. If we
// outgrow it the swap is one URL change — the response shape we touch
// (name, country, admin1, latitude, longitude, timezone) is small.
const GEOCODE_URL = "https://geocoding-api.open-meteo.com/v1/search";

export interface OpenMeteoResult {
  id: number;
  name: string;
  latitude: number;
  longitude: number;
  country?: string;
  country_code?: string;
  admin1?: string;
  timezone?: string;
  population?: number;
}

export function formatCity(r: OpenMeteoResult): string {
  return [r.name, r.admin1, r.country].filter(Boolean).join(", ");
}

/**
 * The city search on its own: a debounced, aborting geocode of `query`.
 *
 * Extracted so a surface that already owns an input — a combo popover's
 * query — can ask the same question without inheriting this picker's field.
 * `enabled: false` parks it (used while an input is showing a picked label
 * rather than a query someone is typing).
 */
export function useCitySearch(
  query: string,
  enabled = true,
): { results: OpenMeteoResult[]; loading: boolean; error: string | null } {
  const [results, setResults] = useState<OpenMeteoResult[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!enabled) return;
    const trimmed = query.trim();
    if (trimmed.length < 2) {
      setResults([]);
      setLoading(false);
      setError(null);
      return;
    }
    const ctrl = new AbortController();
    const t = setTimeout(async () => {
      setLoading(true);
      setError(null);
      try {
        const url = `${GEOCODE_URL}?name=${encodeURIComponent(trimmed)}&count=10&language=en&format=json`;
        const res = await fetch(url, { signal: ctrl.signal });
        if (!res.ok) throw new Error(`geocoding HTTP ${res.status}`);
        const data = (await res.json()) as { results?: OpenMeteoResult[] };
        setResults(data.results ?? []);
      } catch (e) {
        if ((e as { name?: string }).name === "AbortError") return;
        setError(e instanceof Error ? e.message : "geocoding failed");
        setResults([]);
      } finally {
        setLoading(false);
      }
    }, 300);
    return () => {
      clearTimeout(t);
      ctrl.abort();
    };
  }, [query, enabled]);

  return { results, loading, error };
}

export const CityPicker: React.FC<CityPickerProps> = ({
  currentLatitude,
  currentLongitude,
  initialLabel,
  onSelect,
}) => {
  // The input is seeded with whatever the parent already had (the saved
  // city label when editing) so the field shows the active selection
  // instead of being empty. After a pick we replace the query with the
  // formatted "Detroit, Michigan, United States" label.
  const [query, setQuery] = useState(initialLabel ?? "");
  const [results, setResults] = useState<OpenMeteoResult[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  // True iff `query` is currently the label of a successful selection.
  // Used by the search effect to skip auto-searching the label string
  // we just wrote in (which would re-fetch the same city on every pick),
  // and by the focus handler to drop the label on the first keystroke.
  const isShowingPickedLabel = useRef(initialLabel != null && initialLabel !== "");

  // Re-sync the input when initialLabel changes from outside — happens
  // when the parent modal stays mounted and the user clicks a different
  // app's card (so a different saved city label arrives). useState's
  // initializer runs once, so without this the input would still show
  // the previous app's label.
  useEffect(() => {
    setQuery(initialLabel ?? "");
    isShowingPickedLabel.current = initialLabel != null && initialLabel !== "";
  }, [initialLabel]);

  const containerRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // Close on outside click. Mirror of the pattern used in
  // BroadcastEditor's AddContentButton menu.
  useEffect(() => {
    if (!open) return;
    const onDocClick = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", onDocClick);
    return () => document.removeEventListener("mousedown", onDocClick);
  }, [open]);

  // Debounced search. 300ms hits a sweet spot — fast enough to feel
  // responsive, slow enough that typing "Detroit" doesn't fire 7 calls.
  // We also abort in-flight requests so a quick typist doesn't get
  // stale results for an earlier prefix overwriting newer ones.
  useEffect(() => {
    // After a successful pick we display the city label in the input
    // — re-searching that label would just re-show the same city as a
    // dropdown, which is noisy and confusing. Skip until the user
    // actually edits the field.
    if (isShowingPickedLabel.current) return;
    const trimmed = query.trim();
    if (trimmed.length < 2) {
      setResults([]);
      setLoading(false);
      setError(null);
      return;
    }

    const ctrl = new AbortController();
    const t = setTimeout(async () => {
      setLoading(true);
      setError(null);
      try {
        const url = `${GEOCODE_URL}?name=${encodeURIComponent(trimmed)}&count=10&language=en&format=json`;
        const res = await fetch(url, { signal: ctrl.signal });
        if (!res.ok) throw new Error(`geocoding HTTP ${res.status}`);
        const data = (await res.json()) as { results?: OpenMeteoResult[] };
        setResults(data.results ?? []);
        setOpen(true);
      } catch (e) {
        if ((e as { name?: string }).name === "AbortError") return;
        setError(e instanceof Error ? e.message : "geocoding failed");
        setResults([]);
      } finally {
        setLoading(false);
      }
    }, 300);

    return () => {
      clearTimeout(t);
      ctrl.abort();
    };
  }, [query]);

  const handlePick = (r: OpenMeteoResult) => {
    if (!r.timezone) {
      // Open-Meteo always returns timezone, but TypeScript doesn't know
      // that. Belt-and-suspenders: surface it instead of silently filling
      // a timezone-less config that the backend would later choke on.
      setError(`No timezone in geocoding result for ${r.name}`);
      return;
    }
    const sel: CitySelection = {
      name: r.name,
      admin1: r.admin1,
      country: r.country ?? "",
      latitude: r.latitude,
      longitude: r.longitude,
      timezone: r.timezone,
    };
    // Show the picked city in the input itself rather than relying on
    // a separate "Selected" hint — that's the field's actual value now.
    isShowingPickedLabel.current = true;
    setQuery(formatCity(r));
    setResults([]);
    setOpen(false);
    onSelect(sel);
  };

  // Latitude/longitude shown as a small confirmation under the input
  // when something has been picked or pre-loaded — confirms what the
  // city name resolved to (or what's already saved on the row).
  const coordsHint = useMemo(() => {
    if (currentLatitude == null || currentLongitude == null) return null;
    return `${currentLatitude.toFixed(4)}, ${currentLongitude.toFixed(4)}`;
  }, [currentLatitude, currentLongitude]);

  return (
    <div ref={containerRef} className="ds-field" style={{ position: "relative" }}>
      <span>Location *</span>
      <input
        id="city-picker"
        ref={inputRef}
        type="text"
        autoComplete="off"
        value={query}
        placeholder="Search for a city…"
        onChange={(e) => {
          isShowingPickedLabel.current = false;
          setQuery(e.target.value);
          setOpen(true);
        }}
        onFocus={(e) => {
          if (isShowingPickedLabel.current) e.currentTarget.select();
          if (results.length > 0) setOpen(true);
        }}
        className="ds-input"
      />
      {coordsHint && <p className="ds-hint">Coordinates: {coordsHint}</p>}
      {loading && <p className="ds-hint">Searching…</p>}
      {error && <p className="ds-danger-text">{error}</p>}
      {open && results.length > 0 && (
        <ul className="ds-find-pop" style={{ listStyle: "none", margin: 0, padding: 0 }}>
          {results.map((r) => (
            <li key={r.id}>
              <button type="button" className="ds-find-hit" onClick={() => handlePick(r)}>
                <span className="ds-row-copy">
                  <strong>{r.name}</strong>
                  <span>
                    {[r.admin1, r.country].filter(Boolean).join(", ")}
                    {r.timezone ? ` · ${r.timezone}` : ""}
                  </span>
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
};
