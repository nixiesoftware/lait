import type { SignageMedia } from "@/utils/lait/types";
import { THEMES, athanTimes, formatClock } from "./athan";
import { storedContentUrl } from "./model";

type Props = {
  media: SignageMedia | undefined;
  orbit: string | null;
};

export function Thumb({ media, orbit }: Props) {
  if (!media) {
    return <div className="pe-thumb pe-thumb-empty" />;
  }
  if (media.source === "card") {
    return (
      <div
        className="pe-thumb pe-thumb-card"
        style={{
          background: `#${media.background}`,
          color: `#${media.foreground}`,
        }}
      >
        {media.title}
      </div>
    );
  }
  const src = orbit ? storedContentUrl(orbit, media) : null;
  if (media.source === "stored" && src && media.mime.startsWith("image/")) {
    return <img className="pe-thumb" src={src} alt="" />;
  }
  if (media.source === "stored" && src && media.mime.startsWith("video/")) {
    return (
      <video
        className="pe-thumb"
        src={src}
        muted
        playsInline
        preload="metadata"
        onLoadedMetadata={(event) => {
          const video = event.currentTarget;
          if (video.currentTime === 0) video.currentTime = 0.08;
        }}
      />
    );
  }
  if (media.source === "kind" && media.kind === "athan") {
    const day = athanTimes(media.settings);
    const next = day?.prayers[day.next];
    const theme = THEMES[day?.theme ?? "ink"];
    const clock = next
      ? day?.nextIsIqamah && next.iqamah
        ? next.iqamah
        : next.adhan
      : null;
    return (
      <div
        className="pe-thumb pe-thumb-card pe-thumb-athan"
        style={{ background: theme.bg, color: theme.accent }}
      >
        {next && clock
          ? `${day?.nextIsIqamah ? "Iqamah" : next.name} ${formatClock(clock, day?.clock24h ?? true)}`
          : "Athan"}
      </div>
    );
  }
  return (
    <div className="pe-thumb pe-thumb-empty">
      <span>{media.name}</span>
    </div>
  );
}
