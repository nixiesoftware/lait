import { useEffect, useRef } from "react";
import type { SignageMedia } from "@/utils/lait/types";
import {
  formatDuration,
  storedContentUrl,
  type LaidClip,
  type TrimPreview,
} from "./model";
import { ItemMenu, clipMenuItems, type ClipActions } from "./ItemMenu";

type Props = {
  clip: LaidClip | null;
  t: number;
  playing: boolean;
  orbit: string | null;
  trim: TrimPreview | null;
  container?: React.RefObject<HTMLElement | null>;
  actions?: ClipActions;
};

export function Stage({
  clip,
  t,
  playing,
  orbit,
  trim,
  container,
  actions,
}: Props) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const shown = clip;
  const media = shown?.media ?? null;
  const src = orbit && media ? storedContentUrl(orbit, media) : null;
  const intoClip = shown ? Math.max(0, (t - shown.startMs) / 1000) : 0;
  const bleed =
    media?.source === "card" ? `#${media.background}` : "transparent";

  useEffect(() => {
    const video = videoRef.current;
    if (!video || !src) return;
    let target = intoClip;
    if (trim && shown && trim.id === shown.item.id) {
      if (trim.edge === "left") target = 0;
      else target = Math.max(0, trim.durationMs / 1000 - 0.05);
    }
    const drift = Math.abs(video.currentTime - target);
    if (drift > 0.3) video.currentTime = target;
    const live = playing && !trim;
    if (live && video.paused) void video.play().catch(() => {});
    if (!live && !video.paused) video.pause();
  }, [src, intoClip, playing, trim, shown]);

  if (!shown || !media) {
    return (
      <div className="pe-stage">
        <div className="pe-frame">
          <div className="pe-placeholder">
            <strong>Nothing on this program yet</strong>
            Add media on the strip below.
          </div>
        </div>
      </div>
    );
  }

  const frame = (
    <>
      {media.source === "card" ? (
        <div
          className="pe-card"
          style={{
            background: `#${media.background}`,
            color: `#${media.foreground}`,
          }}
        >
          <h1>{media.title}</h1>
          {media.body ? <p>{media.body}</p> : null}
        </div>
      ) : media.source === "stored" && src && media.mime.startsWith("image/") ? (
        <img src={src} alt={media.name} />
      ) : media.source === "stored" && src && media.mime.startsWith("video/") ? (
        <video ref={videoRef} src={src} playsInline muted />
      ) : (
        <div className="pe-placeholder">
          <strong>{media.name}</strong>
          {kindLabel(media)}
          {src ? null : "This entry has no bytes this head can show."}
        </div>
      )}
      {trim && trim.id === shown.item.id ? (
        <div className="pe-trim-flag">
          <b>{trim.edge === "left" ? "Start" : "End"}</b>
          <span>{formatDuration(trim.durationMs)}</span>
        </div>
      ) : null}
    </>
  );

  return (
    <div className="pe-stage">
      <div className="pe-bleed" style={{ background: bleed }} aria-hidden />
      {actions ? (
        <ItemMenu
          className="pe-frame"
          items={clipMenuItems(shown, actions)}
          container={container}
        >
          {frame}
        </ItemMenu>
      ) : (
        <div className="pe-frame">{frame}</div>
      )}
    </div>
  );
}

function kindLabel(media: SignageMedia): string {
  switch (media.source) {
    case "stored":
      return media.mime;
    case "kind":
      return media.kind;
    case "live":
      return media.resource;
    default:
      return media.source;
  }
}
