/**
 * Files — what programs are made of.
 *
 * A file wears its use: faint when no program holds it, a dot of now when a
 * screen is showing it. Dropping files on the page uploads them; deleting one
 * is a press, and the bar at the foot offers to put it back — with a word
 * about the programs that still name it, because that is the thing a person
 * would have wanted a warning about.
 */

import React, { useMemo, useRef, useState } from "react";
import { useSearch } from "@tanstack/react-router";
import { useDropzone } from "react-dropzone";
import { Images, Plus, Trash2, Upload } from "lucide-react";
import {
  CatalogueRow,
  Chips,
  CommitText,
  Empty,
  GalleryShot,
  Inspector,
  Page,
  PageHeader,
  PageSearch,
  PageStatus,
  SelectionBar,
  ViewToggle,
  haptic,
  litProps,
  useFocus,
  useHoldable,
  useLive,
  useOrbit,
  useToast,
  useUndo,
  useWide,
  type MenuItem,
} from "@/ds";
import { adopt, current, putMedia, removeMedia, useFleet } from "@/utils/screens/fleet";
import {
  type ContentItemProps,
  type SourceCategory,
  sourceCategory,
  sourceLabel,
} from "@/utils/content/library";
import { Thumb } from "@/program-editor/Thumb";
import { formatDuration } from "@/program-editor/model";
import { useCreateBroadcast } from "@/utils/navigation/hooks";
import { uploadContentAll } from "@/utils/content/api";
import type { SignageMedia } from "@/utils/lait/types";

const CATEGORY_ORDER: SourceCategory[] = ["image", "video", "card", "kind", "live", "stored"];

const CATEGORY_LABEL: Record<SourceCategory, string> = {
  image: "Images",
  video: "Videos",
  card: "Cards",
  kind: "Apps",
  live: "Live",
  stored: "Other",
};

export const ContentListPage: React.FC = () => {
  const fleet = useFleet();
  const { now } = useLive();
  const { held } = useFocus();
  const undo = useUndo();

  /** Which programs hold a file, and whether any screen is showing it now. */
  const usageOf = (id: string) => {
    const holders = fleet.programs.filter((program) => program.items.some((item) => item.media === id));
    const holderIds = new Set(holders.map((program) => program.id));
    const onGlass = fleet.screens.some((screen) => {
      const showing = fleet.playbackFor(screen, now).showing;
      return showing.showing === "program" && holderIds.has(showing.program);
    });
    return { holders, onGlass };
  };

  const { q: searchQuery } = useSearch({ strict: false }) as { q?: string };
  const toast = useToast();
  const orbit = useOrbit();
  const wide = useWide();
  const fileInputRef = useRef<HTMLInputElement>(null);

  const [query, setQuery] = useState(searchQuery || "");
  const [viewMode, setViewMode] = useState<"grid" | "list">("grid");
  const [uploading, setUploading] = useState<ContentItemProps[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [filter, setFilter] = useState<"all" | SourceCategory>("all");
  const [inspect, setInspect] = useState<string | null>(null);

  const { handleCreate: handleCreateProgram, isCreating: isCreatingProgram } =
    useCreateBroadcast();

  const items: ContentItemProps[] = fleet.media;

  const refused = (what: string) => (err: unknown) => {
    haptic("error");
    toast.show(what, err instanceof Error ? err.message : String(err));
  };

  const uploadFiles = async (files: File[]) => {
    if (files.length === 0) return;
    const stamp = Date.now();
    const placeholders: ContentItemProps[] = files.map((file, i) => ({
      id: `upload-${stamp}-${i}`,
      tempId: `upload-${stamp}-${i}`,
      name: file.name.replace(/\.[^/.]+$/, ""),
      source: "stored",
      content: "",
      size: file.size,
      mime: file.type,
      duration_ms: null,
      width: null,
      height: null,
      isUploading: true,
    }));
    setUploading(placeholders);
    try {
      const outcome = await uploadContentAll(files);
      if (outcome.uploaded.length > 0) {
        adopt({ media: [...outcome.uploaded, ...current().media] });
      }
      if (outcome.refused.length > 0) {
        toast.show(
          "Some files were refused",
          outcome.refused.map((row) => row.reason).join(" "),
        );
        haptic("error");
      } else if (outcome.uploaded.length > 0) {
        haptic("save");
      }
    } catch (err) {
      toast.show("Upload failed", (err as Error).message);
      haptic("error");
    } finally {
      setUploading([]);
    }
  };

  const { getRootProps, getInputProps, isDragActive } = useDropzone({
    noClick: true,
    noKeyboard: true,
    accept: { "image/*": [], "video/*": [] },
    onDrop: (files) => {
      void uploadFiles(files);
    },
  });

  const remove = (ids: string[]) => {
    haptic("delete");
    setSelected(new Set());
    if (inspect && ids.includes(inspect)) setInspect(null);
    const holders = new Set(
      ids.flatMap((id) => usageOf(id).holders.map((program) => program.name)),
    );
    void Promise.all(ids.map((id) => removeMedia(id)))
      .then((gone) => {
        const kept = gone.filter((row): row is SignageMedia => row != null);
        if (kept.length === 0) return;
        const what = kept.length === 1 ? `Deleted ${kept[0].name}` : `Deleted ${kept.length} files`;
        const still =
          holders.size > 0 ? ` — still named by ${[...holders].join(", ")}` : "";
        undo.offer(`${what}${still}`, () => Promise.all(kept.map((row) => putMedia(row))));
      })
      .catch(refused("Could not delete"));
  };

  const toggle = (id: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const openInspect = (item: ContentItemProps) => {
    if (item.isUploading) return;
    setInspect(item.id);
  };

  const availableTypes = CATEGORY_ORDER.filter((c) =>
    items.some((item) => sourceCategory(item) === c),
  );

  const displayItems = useMemo(() => {
    const q = query.trim().toLowerCase();
    const rows = items.filter((item) => {
      if (q && !item.name.toLowerCase().includes(q)) return false;
      if (filter !== "all" && sourceCategory(item) !== filter) return false;
      return true;
    });
    rows.sort((a, b) => a.name.localeCompare(b.name));
    return [...uploading, ...rows];
  }, [items, uploading, query, filter]);

  const menuFor = (item: ContentItemProps): MenuItem[] => [
    { label: "Details", onPick: () => openInspect(item) },
    { label: "Delete", danger: true, onPick: () => remove([item.id]) },
  ];

  const showList = wide && viewMode === "list";
  const inspected = inspect ? items.find((row) => row.id === inspect) ?? null : null;

  return (
    <Page>
      <div {...getRootProps()}>
        <input {...getInputProps()} />
        <PageHeader title="Files" icon={<Images size={20} />}>
          <button
            type="button"
            className="ds-btn ds-btn-solid"
            onClick={() => fileInputRef.current?.click()}
          >
            <Upload size={16} />
            Upload
          </button>
          <button
            type="button"
            className="ds-btn ds-btn-ghost"
            disabled={isCreatingProgram}
            onClick={() => void handleCreateProgram()}
          >
            <Plus size={16} />
            New program
          </button>
        </PageHeader>

        <input
          ref={fileInputRef}
          type="file"
          multiple
          accept="image/*,video/*"
          hidden
          onChange={(event) => {
            const files = Array.from(event.target.files || []);
            event.target.value = "";
            void uploadFiles(files);
          }}
        />

        <PageStatus loading={fleet.loading} error={fleet.error ?? ""} />

        {selected.size > 0 ? (
          <SelectionBar count={selected.size} onClear={() => setSelected(new Set())}>
            <button
              type="button"
              className="ds-btn ds-btn-danger"
              onClick={() => remove(Array.from(selected))}
            >
              <Trash2 size={16} />
              Delete
            </button>
          </SelectionBar>
        ) : (
          items.length > 0 && (
            <div className="ds-toolbar">
              <PageSearch value={query} onChange={setQuery} placeholder="Filter media…" />
              <Chips
                value={filter}
                onChange={setFilter}
                items={[
                  { id: "all" as const, label: "All" },
                  ...availableTypes.map((id) => ({ id, label: CATEGORY_LABEL[id] })),
                ]}
              />
              <ViewToggle value={viewMode} onChange={setViewMode} />
            </div>
          )
        )}

        {isDragActive && (
          <p className="ds-hint" style={{ marginBottom: 12 }}>
            Drop to upload
          </p>
        )}

        {items.length === 0 && !fleet.loading && uploading.length === 0 ? (
          <Empty title="Drop images or videos here, or upload.">
            <button
              type="button"
              className="ds-btn ds-btn-solid"
              onClick={() => fileInputRef.current?.click()}
            >
              <Upload size={16} />
              Upload
            </button>
          </Empty>
        ) : showList ? (
          <div className="ds-rows">
            {displayItems.map((item) => (
              <Star
                key={item.tempId || item.id}
                id={item.id}
                use={item.isUploading || fleet.loading ? null : usageOf(item.id)}
                held={held}
              >
                <CatalogueRow
                  name={item.name}
                  meta={
                    item.isUploading
                      ? "Uploading…"
                      : [
                          sourceLabel(item),
                          item.width && item.height ? `${item.width}×${item.height}` : null,
                        ]
                          .filter(Boolean)
                          .join(" · ")
                  }
                  selected={selected.has(item.id)}
                  onSelect={() => toggle(item.id)}
                  onOpen={() => openInspect(item)}
                  menu={item.isUploading ? [] : menuFor(item)}
                  more={item.isUploading ? [] : menuFor(item)}
                  disabled={item.isUploading}
                >
                  <Thumb media={item.isUploading ? undefined : item} orbit={orbit} />
                </CatalogueRow>
              </Star>
            ))}
          </div>
        ) : (
          <div className="ds-gallery">
            {displayItems.map((item) => (
              <Star
                key={item.tempId || item.id}
                id={item.id}
                use={item.isUploading || fleet.loading ? null : usageOf(item.id)}
                held={held}
              >
                <GalleryShot
                  name={item.isUploading ? "Uploading…" : item.name}
                  badge={item.isUploading ? undefined : sourceLabel(item)}
                  play={
                    !item.isUploading && item.source === "stored" && item.mime.startsWith("video/")
                  }
                  selected={selected.has(item.id)}
                  onSelect={() => toggle(item.id)}
                  onOpen={() => openInspect(item)}
                  menu={item.isUploading ? [] : menuFor(item)}
                  more={item.isUploading ? [] : menuFor(item)}
                  disabled={item.isUploading}
                >
                  <Thumb media={item.isUploading ? undefined : item} orbit={orbit} />
                </GalleryShot>
              </Star>
            ))}
          </div>
        )}
      </div>

      <Inspector
        open={inspected != null}
        onOpenChange={(open) => {
          if (!open) setInspect(null);
        }}
        title={inspected?.name ?? "Media"}
        actions={
          inspected && (
            <button
              type="button"
              className="ds-btn ds-btn-danger"
              onClick={() => remove([inspected.id])}
            >
              Delete
            </button>
          )
        }
      >
        {inspected && (
          <>
            <div className="ds-tile-media" style={{ borderRadius: 10, marginBottom: 12 }}>
              <Thumb media={inspected} orbit={orbit} />
            </div>
            <CommitText
              label="Name"
              value={inspected.name}
              onWrite={(next) =>
                next.trim() && next.trim() !== inspected.name
                  ? putMedia({ ...inspected, name: next.trim() })
                  : Promise.resolve()
              }
            />
            <p className="ds-hint">Type · {sourceLabel(inspected)}</p>
            <p className="ds-hint">
              Size · {inspected.width && inspected.height ? `${inspected.width}×${inspected.height}` : "unknown"}
            </p>
            {inspected.duration_ms != null && (
              <p className="ds-hint">Duration · {formatDuration(inspected.duration_ms)}</p>
            )}
            {inspected.source === "stored" && (
              <p className="ds-hint">File · {(inspected.size / (1024 * 1024)).toFixed(1)} MB</p>
            )}
            {(() => {
              const use = usageOf(inspected.id);
              return (
                <p className="ds-hint">
                  {use.holders.length === 0
                    ? "Held by no program."
                    : `Held by ${use.holders.map((program) => program.name).join(", ")}.`}
                </p>
              );
            })()}
          </>
        )}
      </Inspector>
    </Page>
  );
};

/**
 * A file, wearing its use: faint when no program holds it, a dot of now when a
 * screen is showing it. Holding it lights the programs that hold it elsewhere.
 */
function Star({
  id,
  use,
  held,
  children,
}: {
  id: string;
  use: { holders: { id: string }[]; onGlass: boolean } | null;
  held: ReturnType<typeof useFocus>["held"];
  children: React.ReactNode;
}) {
  const hold = useHoldable("file", id);
  const related =
    held?.kind === "program" && use != null && use.holders.some((program) => program.id === held.id);
  return (
    <div
      className="ds-star"
      data-unused={use != null && use.holders.length === 0 ? "true" : undefined}
      data-onglass={use?.onGlass ? "true" : undefined}
      {...hold.bind}
      {...(hold.held ? {} : litProps(held, related))}
    >
      {children}
      {use?.onGlass && <span className="ds-star-now" aria-label="Showing now" />}
    </div>
  );
}
