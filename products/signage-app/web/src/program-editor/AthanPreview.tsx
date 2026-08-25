import { THEMES, athanTimes, formatClock } from "./athan";

export function AthanPreview({ settings }: { settings: Record<string, string> }) {
  const day = athanTimes(settings);
  if (!day) {
    return (
      <div className="pe-placeholder">
        <strong>Athan</strong>
        Pick a city to compute times.
      </div>
    );
  }
  const theme = THEMES[day.theme];
  return (
    <div
      className="pe-athan"
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
