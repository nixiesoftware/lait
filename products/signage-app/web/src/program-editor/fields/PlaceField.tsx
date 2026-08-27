/**
 * A location: coordinates and the zone they are read in.
 *
 * Search is an accelerator, not the only door. The previous form hid latitude
 * and longitude whenever a city picker was present, so a screen on a closed
 * network — the deployment this product is for — could not be configured at
 * all. Manual entry is the field; search fills it in.
 *
 * The zone is required and never defaulted here. Coordinates with no zone
 * produce a plausible timetable for the wrong offset, which is the failure that
 * looks like nothing being wrong.
 */

import { useState } from "react";
import { MapPin, Search } from "lucide-react";
import { CityPicker, type CitySelection } from "./CityPicker";
import type { PlaceField as PlaceFieldSpec } from "../kinds/types";
import { Row, type Surface } from "./FieldControl";

const ZONE_HINT = "IANA name, e.g. Europe/London";

export function PlaceField({
  field,
  draft,
  onPatch,
  surface,
  errorFor,
}: {
  field: PlaceFieldSpec;
  draft: Record<string, string>;
  onPatch: (patch: Record<string, string>) => void;
  surface: Surface;
  errorFor: (key: string) => string | null;
}) {
  const [searching, setSearching] = useState(false);
  const { latitude, longitude, timezone } = field.keys;

  const pick = (city: CitySelection) => {
    onPatch({
      [latitude]: String(city.latitude),
      [longitude]: String(city.longitude),
      [timezone]: city.timezone,
    });
    setSearching(false);
  };

  const useThisDevice = () => {
    if (typeof navigator === "undefined" || !("geolocation" in navigator)) return;
    navigator.geolocation.getCurrentPosition(
      (position) => {
        // The browser knows where, never in which zone — so the zone this
        // device is set to is the honest guess, and it stays editable.
        const guess = Intl.DateTimeFormat().resolvedOptions().timeZone ?? "";
        onPatch({
          [latitude]: position.coords.latitude.toFixed(4),
          [longitude]: position.coords.longitude.toFixed(4),
          ...(draft[timezone]?.trim() ? {} : { [timezone]: guess }),
        });
      },
      () => {},
      { timeout: 5000 },
    );
  };

  return (
    <div className={`pe-field is-block is-place is-${surface}`}>
      <span className="pe-field-label">
        {field.label}
        {field.hint ? <small>{field.hint}</small> : null}
      </span>

      <div className="pe-place-actions">
        <button
          type="button"
          className="ds-btn ds-btn-quiet"
          onClick={() => setSearching((open) => !open)}
        >
          <Search size={15} />
          Search a city
        </button>
        <button type="button" className="ds-btn ds-btn-quiet" onClick={useThisDevice}>
          <MapPin size={15} />
          Use this device
        </button>
      </div>

      {searching ? (
        <div className="pe-place-search">
          <CityPicker
            currentLatitude={draft[latitude] ? Number(draft[latitude]) : null}
            currentLongitude={draft[longitude] ? Number(draft[longitude]) : null}
            initialLabel={null}
            onSelect={pick}
          />
          <p className="ds-hint">
            Search asks a geocoding service on the internet. Typing the
            coordinates below never leaves this device.
          </p>
        </div>
      ) : null}

      <div className="pe-place-grid">
        <Row label="Latitude" error={errorFor(latitude)} surface={surface}>
          <input
            className="ds-input"
            inputMode="decimal"
            placeholder="51.5074"
            value={draft[latitude] ?? ""}
            onChange={(event) => onPatch({ [latitude]: event.target.value })}
          />
        </Row>
        <Row label="Longitude" error={errorFor(longitude)} surface={surface}>
          <input
            className="ds-input"
            inputMode="decimal"
            placeholder="-0.1278"
            value={draft[longitude] ?? ""}
            onChange={(event) => onPatch({ [longitude]: event.target.value })}
          />
        </Row>
      </div>

      <Row label="Time zone" hint={ZONE_HINT} error={errorFor(timezone)} surface={surface}>
        <input
          className="ds-input"
          list="pe-zones"
          placeholder="Europe/London"
          value={draft[timezone] ?? ""}
          onChange={(event) => onPatch({ [timezone]: event.target.value })}
        />
      </Row>
      <ZoneOptions />
    </div>
  );
}

/**
 * Whatever this browser can enumerate, offered as completions. It is a
 * convenience over a free-text field, never the gate — an unknown zone is
 * refused by the panel, not hidden by the picker.
 */
function ZoneOptions() {
  const zones =
    typeof Intl.supportedValuesOf === "function"
      ? Intl.supportedValuesOf("timeZone")
      : [];
  if (zones.length === 0) return null;
  return (
    <datalist id="pe-zones">
      {zones.map((zone) => (
        <option key={zone} value={zone} />
      ))}
    </datalist>
  );
}
