import type { SignageMedia } from "@/utils/lait/types";
import { panelFor } from "./kinds/registry";
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
  if (media.source === "kind") {
    const kindPanel = panelFor(media.kind);
    if (kindPanel) {
      return (
        <div className="pe-thumb pe-thumb-card pe-thumb-kind">
          <kindPanel.Preview settings={media.settings} density="thumb" />
        </div>
      );
    }
  }
  return (
    <div className="pe-thumb pe-thumb-empty">
      <span>{media.name}</span>
    </div>
  );
}
