/**
 * Kind classification helpers.
 *
 * The backend stores `kind` (the Definition name from
 * backend/integrations/) instead of the legacy `type`. These helpers
 * map kind values to UI category buckets:
 *   - image:           kind === "image"
 *   - video:           kind === "library_video" (or any future
 *                      video-flavored kind)
 *   - html / iframe:   kind === "html_widget" or any server-rendered
 *                      kind that produces HTML — we treat every kind
 *                      whose Resolve emits Payload.Kind="html" as
 *                      "html-shaped" for UI purposes
 *
 * The functions still accept a string and tolerate the legacy values
 * ("image", "video", "html", "image/png", etc.) so any spot that hasn't
 * been migrated to read `content.kind` doesn't immediately break.
 */

const VIDEO_KINDS = new Set(['library_video', 'video']);
const HTML_KINDS = new Set(['html_widget', 'html', 'athan']);

export function isImageContent(kind: string): boolean {
  if (!kind) return false;
  return kind === 'image' || kind.startsWith('image/');
}

export function isVideoContent(kind: string): boolean {
  if (!kind) return false;
  return VIDEO_KINDS.has(kind) || kind.startsWith('video/');
}

export function isHtmlContent(kind: string): boolean {
  if (!kind) return false;
  return HTML_KINDS.has(kind);
}

/**
 * isYouTubeContent is split out from isHtmlContent because the broadcast
 * editor's preview surfaces want to render a YouTube embed differently
 * from a generic HTML iframe (autoplay rules, controls overlay, etc.).
 * Today both still navigate Chromium to the URL, but keeping the split
 * lets future preview UIs branch.
 */
export function isYouTubeContent(kind: string): boolean {
  return kind === 'youtube';
}
