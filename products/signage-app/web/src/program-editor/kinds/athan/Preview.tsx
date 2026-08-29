import type { Density } from "../types";
import { THEMES, athanTimes, formatClock } from "./compute";

/**
 * One preview, three sizes. The stage, the inspector and the filmstrip
 * thumbnail were each deriving this separately; a card that disagrees with
 * itself across the editor is a card nobody can trust against the screen.
 */
export function AthanPreview({
  settings,
  density = "stage",
}: {
  settings: Record<string, string>;
  density?: Density;
}) {
  const day = athanTimes(settings);
  if (!day) {
    return (
      <div className={`pe-athan is-empty is-${density}`}>
        <strong>Athan</strong>
        <span>Pick a location to compute times.</span>
      </div>
    );
  }
  const theme = THEMES[day.theme];

  if (density === "thumb") {
    const next = day.prayers[day.next];
    const clock = next ? (day.nextIsIqamah && next.iqamah ? next.iqamah : next.adhan) : null;
    return (
      <div
        className="pe-athan is-thumb"
        style={{ background: theme.bg, color: theme.accent }}
      >
        {next && clock
          ? `${day.nextIsIqamah ? "Iqamah" : next.name} ${formatClock(clock, day.clock24h)}`
          : "Athan"}
      </div>
    );
  }

  return (
    <div
      className={`pe-athan is-${density}`}
      style={{ background: theme.bg, color: theme.accent }}
    >
      <header>
        <strong>Athan</strong>
        <span style={{ color: theme.muted }}>
          {day.nowLabel} · {day.zone}
          {day.showHijri && day.hijriLabel ? ` · ${day.hijriLabel}` : ""}
        </span>
      </header>
      <ol>
        {day.prayers.map((prayer, index) => (
          <li
            key={prayer.name}
            className={index === day.next ? "is-next" : undefined}
            style={{ color: index === day.next ? theme.accent : theme.muted }}
          >
            <span>{prayer.name}</span>
            <span className="pe-athan-times">
              {formatClock(prayer.adhan, day.clock24h)}
              {day.showIqamah ? (
                <em>{prayer.iqamah ? formatClock(prayer.iqamah, day.clock24h) : "—"}</em>
              ) : null}
            </span>
          </li>
        ))}
      </ol>
    </div>
  );
}
