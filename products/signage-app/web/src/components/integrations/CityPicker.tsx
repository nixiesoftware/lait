import React, { useEffect, useMemo, useRef, useState } from "react";
import Label from "@/components/form/Label";

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

interface OpenMeteoResult {
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

function formatCity(r: OpenMeteoResult): string {
  return [r.name, r.admin1, r.country].filter(Boolean).join(", ");
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
    <div ref={containerRef} className="relative">
      <Label htmlFor="city-picker">
        Location <span className="text-red-500">*</span>
      </Label>
      <input
        id="city-picker"
        ref={inputRef}
        type="text"
        autoComplete="off"
        value={query}
        placeholder="Search for a city…"
        onChange={(e) => {
          // First user-driven change after a pick replaces the displayed
          // label with their typing — clear the "this is a picked label"
          // flag so the search effect runs again.
          isShowingPickedLabel.current = false;
          setQuery(e.target.value);
          setOpen(true);
        }}
        onFocus={(e) => {
          // Highlight the current value on focus so the user's first
          // keystroke replaces it. If they don't type, the picked city
          // stays visible — matches typical browser select-on-focus UX.
          if (isShowingPickedLabel.current) e.currentTarget.select();
          if (results.length > 0) setOpen(true);
        }}
        className="h-11 w-full rounded-md border border-gray-300 bg-transparent px-4 py-2.5 text-sm shadow-theme-xs placeholder:text-gray-400 focus:border-brand-300 focus:outline-hidden focus:ring-3 focus:ring-brand-500/10 dark:border-gray-700 dark:bg-gray-900 dark:text-white/90 dark:placeholder:text-white/30 dark:focus:border-brand-800"
      />
      {coordsHint && (
        <p className="mt-1.5 text-xs text-gray-500 dark:text-gray-400">
          Coordinates: {coordsHint}
        </p>
      )}
      {loading && (
        <p className="mt-1.5 text-xs text-gray-500 dark:text-gray-400">Searching…</p>
      )}
      {error && (
        <p className="mt-1.5 text-xs text-red-600 dark:text-red-400">{error}</p>
      )}
      {open && results.length > 0 && (
        <ul className="absolute z-50 mt-1 max-h-72 w-full overflow-auto rounded-md border border-gray-200 bg-white shadow-lg dark:border-gray-700 dark:bg-gray-900">
          {results.map((r) => (
            <li
              key={r.id}
              onClick={() => handlePick(r)}
              className="cursor-pointer px-3 py-2 text-sm hover:bg-brand-50 dark:hover:bg-brand-950/40"
            >
              <div className="font-medium text-gray-900 dark:text-white/90">
                {r.name}
              </div>
              <div className="text-xs text-gray-500 dark:text-gray-400">
                {[r.admin1, r.country].filter(Boolean).join(", ")}
                {r.timezone ? ` · ${r.timezone}` : ""}
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
};
