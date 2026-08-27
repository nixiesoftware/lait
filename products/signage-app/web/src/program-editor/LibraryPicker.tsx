import { useMemo, useRef, useState } from "react";
import type { SignageMedia } from "@/utils/lait/types";
import { uploadContentAll } from "@/utils/content/api";
import { Thumb } from "./Thumb";

type Filter = "all" | "image" | "video" | "card";

type Props = {
  library: SignageMedia[];
  orbit: string | null;
  onAdd: (media: SignageMedia) => void;
  onUploaded: (media: SignageMedia[]) => void;
  variant: "grid" | "list";
  onUploadError?: (message: string) => void;
};

function matches(media: SignageMedia, filter: Filter, query: string): boolean {
  if (query && !media.name.toLowerCase().includes(query)) return false;
  if (filter === "all") return true;
  if (filter === "card") return media.source === "card";
  if (media.source !== "stored") return false;
  if (filter === "image") return media.mime.startsWith("image/");
  return media.mime.startsWith("video/");
}

export function LibraryPicker({
  library,
  orbit,
  onAdd,
  onUploaded,
  variant,
  onUploadError,
}: Props) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase();
    return library.filter((entry) => matches(entry, filter, q));
  }, [library, filter, query]);

  const upload = async (files: FileList | null) => {
    if (!files || files.length === 0) return;
    setBusy(true);
    try {
      const outcome = await uploadContentAll([...files]);
      onUploaded(outcome.uploaded);
      if (outcome.refused.length > 0) {
        onUploadError?.(outcome.refused.map((row) => row.reason).join(" "));
      }
    } catch (err) {
      onUploadError?.(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <input
        className="ds-search"
        value={query}
        placeholder="Search the library"
        onChange={(event) => setQuery(event.target.value)}
        aria-label="Search the library"
      />
      <div className="ds-chips">
        {(["all", "image", "video", "card"] as const).map((key) => (
          <button
            type="button"
            key={key}
            className={`ds-chip${filter === key ? " is-on" : ""}`}
            onClick={() => setFilter(key)}
          >
            {key === "all" ? "All" : key === "image" ? "Images" : key === "video" ? "Videos" : "Cards"}
          </button>
        ))}
      </div>
      <button
        type="button"
        className="ds-row"
        disabled={busy}
        onClick={() => inputRef.current?.click()}
      >
        {busy ? "Uploading…" : "Upload image or video"}
      </button>
      <input
        ref={inputRef}
        type="file"
        accept="image/*,video/*"
        multiple
        hidden
        onChange={(event) => {
          void upload(event.target.files);
          event.target.value = "";
        }}
      />
      {rows.length === 0 ? (
        <p className="ds-empty">{busy ? "Uploading…" : "Nothing matches."}</p>
      ) : variant === "grid" ? (
        <div className="ds-tile-grid">
          {rows.map((entry) => (
            <button
              type="button"
              key={entry.id}
              className="ds-tile"
              onClick={() => onAdd(entry)}
            >
              <span className="ds-tile-media">
                <Thumb media={entry} orbit={orbit} />
                <span className="ds-tile-plus" aria-hidden>
                  <i>+</i>
                </span>
              </span>
              <span className="ds-tile-name">{entry.name}</span>
            </button>
          ))}
        </div>
      ) : (
        rows.map((entry) => (
          <button
            type="button"
            key={entry.id}
            className="ds-row pe-lib-row"
            onClick={() => onAdd(entry)}
          >
            <Thumb media={entry} orbit={orbit} />
            <span className="ds-row-copy">
              {entry.name}
              <span>{entry.source === "stored" ? entry.mime : entry.source}</span>
            </span>
          </button>
        ))
      )}
    </>
  );
}
