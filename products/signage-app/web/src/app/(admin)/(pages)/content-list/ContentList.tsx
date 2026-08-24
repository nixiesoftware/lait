import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useSearch } from "@tanstack/react-router";
import { useDropzone } from "react-dropzone";
import { Images, Plus, Trash2, Upload } from "lucide-react";
import {
  CatalogueRow,
  Chips,
  Confirm,
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
  useOrbit,
  useToast,
  useWide,
  type MenuItem,
} from "@/ds";
import {
  type ContentItemProps,
  type SourceCategory,
  sourceCategory,
  sourceLabel,
} from "@/components/content";
import { Thumb } from "@/program-editor/Thumb";
import { formatDuration } from "@/program-editor/model";
import { useCreateBroadcast } from "@/utils/navigation/hooks";
import {
  deleteMedia,
  fetchLibrary,
  fetchMediaUsedBy,
  saveMedia,
  uploadContentAll,
} from "@/utils/content/api";
import { fetchPrograms } from "@/utils/broadcasts/api";

const CATEGORY_ORDER: SourceCategory[] = [
  "image",
  "video",
  "card",
  "kind",
  "live",
  "stored",
];

const CATEGORY_LABEL: Record<SourceCategory, string> = {
  image: "Images",
  video: "Videos",
  card: "Cards",
  kind: "Apps",
  live: "Live",
  stored: "Other",
};

export const ContentListPage: React.FC = () => {
  const { q: searchQuery } = useSearch({ strict: false }) as { q?: string };
  const toast = useToast();
  const orbit = useOrbit();
  const wide = useWide();
  const fileInputRef = useRef<HTMLInputElement>(null);

  const [query, setQuery] = useState(searchQuery || "");
  const [items, setItems] = useState<ContentItemProps[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [viewMode, setViewMode] = useState<"grid" | "list">("grid");
  const [uploading, setUploading] = useState<ContentItemProps[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [filter, setFilter] = useState<"all" | SourceCategory>("all");
  const [inspect, setInspect] = useState<ContentItemProps | null>(null);
  const [rename, setRename] = useState("");
  const [deleteIds, setDeleteIds] = useState<string[] | null>(null);
  const [deleteUsedBy, setDeleteUsedBy] = useState<string[]>([]);

  useEffect(() => {
    setQuery(searchQuery || "");
  }, [searchQuery]);

  const { handleCreate: handleCreateProgram, isCreating: isCreatingProgram } =
    useCreateBroadcast();

  const load = useCallback(() => {
    setLoading(true);
    fetchLibrary()
      .then(setItems)
      .catch((err) => {
        setError((err as Error).message || "Failed to load the library");
      })
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

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
      load();
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

  const offerDelete = async (ids: string[]) => {
    try {
      const usedBy = await Promise.all(ids.map((id) => fetchMediaUsedBy(id)));
      const programIds = [...new Set(usedBy.flat())];
      let names: string[] = [];
      if (programIds.length > 0) {
        const programs = await fetchPrograms();
        names = programIds.map(
          (pid) => programs.find((p) => p.id === pid)?.name ?? pid,
        );
      }
      setDeleteUsedBy(names);
    } catch (err) {
      toast.show(
        "Could not check which programs use this media",
        (err as Error).message,
      );
      haptic("error");
      return;
    }
    setDeleteIds(ids);
  };

  const confirmDelete = async () => {
    if (!deleteIds) return;
    try {
      for (const id of deleteIds) await deleteMedia(id);
      setSelected(new Set());
      if (inspect && deleteIds.includes(inspect.id)) setInspect(null);
      haptic("delete");
      load();
    } catch (err) {
      toast.show("Failed to delete media", (err as Error).message);
      haptic("error");
    } finally {
      setDeleteIds(null);
      setDeleteUsedBy([]);
    }
  };

  const updateName = async (id: string, name: string) => {
    const item = items.find((row) => row.id === id);
    if (!item || !name.trim()) return;
    try {
      const { isUploading: _u, tempId: _t, ...media } = item;
      await saveMedia({ ...media, name: name.trim() });
      if (inspect?.id === id) setInspect({ ...inspect, name: name.trim() });
      haptic("save");
      load();
    } catch (err) {
      toast.show("Failed to rename media", (err as Error).message);
      haptic("error");
    }
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
    setInspect(item);
    setRename(item.name);
  };

  const availableTypes = CATEGORY_ORDER.filter((c) =>
    items.some((item) => sourceCategory(item) === c),
  );

  const displayItems = useMemo(() => {
    const q = query.trim().toLowerCase();
    let rows = items.filter((item) => {
      if (q && !item.name.toLowerCase().includes(q)) return false;
      if (filter !== "all" && sourceCategory(item) !== filter) return false;
      return true;
    });
    rows.sort((a, b) => a.name.localeCompare(b.name));
    return [...uploading, ...rows];
  }, [items, uploading, query, filter]);

  const menuFor = (item: ContentItemProps): MenuItem[] => [
    { label: "Details", onPick: () => openInspect(item) },
    {
      label: "Delete",
      danger: true,
      onPick: () => {
        void offerDelete([item.id]);
      },
    },
  ];

  const usedByNote =
    deleteUsedBy.length > 0
      ? ` Still playing in: ${deleteUsedBy.join(", ")}.`
      : "";

  const showList = wide && viewMode === "list";

  return (
    <Page>
      <div {...getRootProps()}>
        <input {...getInputProps()} />
        <PageHeader title="Media" icon={<Images size={20} />}>
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
            {isCreatingProgram ? "Creating…" : "New program"}
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

        <PageStatus loading={loading && items.length === 0} error={error} />

        {selected.size > 0 ? (
          <SelectionBar
            count={selected.size}
            onClear={() => setSelected(new Set())}
          >
            <button
              type="button"
              className="ds-btn ds-btn-danger"
              onClick={() => void offerDelete(Array.from(selected))}
            >
              <Trash2 size={16} />
              Delete
            </button>
          </SelectionBar>
        ) : (
          <div className="ds-toolbar">
            <PageSearch
              value={query}
              onChange={setQuery}
              placeholder="Filter media…"
            />
            <Chips
              value={filter}
              onChange={setFilter}
              items={[
                { id: "all" as const, label: "All" },
                ...availableTypes.map((id) => ({
                  id,
                  label: CATEGORY_LABEL[id],
                })),
              ]}
            />
            <ViewToggle value={viewMode} onChange={setViewMode} />
          </div>
        )}

        {isDragActive && (
          <p className="ds-hint" style={{ marginBottom: 12 }}>
            Drop to upload
          </p>
        )}

        {items.length === 0 && !loading && uploading.length === 0 ? (
          <Empty title="Drop images or videos here, or upload.">
            <button
              type="button"
              className="ds-btn ds-btn-solid"
              onClick={() => fileInputRef.current?.click()}
            >
              Upload
            </button>
          </Empty>
        ) : showList ? (
          <div className="ds-rows">
            {displayItems.map((item) => (
              <CatalogueRow
                key={item.tempId || item.id}
                name={item.name}
                meta={
                  item.isUploading
                    ? "Uploading…"
                    : [
                        sourceLabel(item),
                        item.width && item.height
                          ? `${item.width}×${item.height}`
                          : null,
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
            ))}
          </div>
        ) : (
          <div className="ds-gallery">
            {displayItems.map((item) => (
              <GalleryShot
                key={item.tempId || item.id}
                name={item.isUploading ? "Uploading…" : item.name}
                badge={item.isUploading ? undefined : sourceLabel(item)}
                play={
                  !item.isUploading &&
                  item.source === "stored" &&
                  item.mime.startsWith("video/")
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
            ))}
          </div>
        )}
      </div>

      <Inspector
        open={inspect != null}
        onOpenChange={(open) => {
          if (!open) setInspect(null);
        }}
        title={inspect?.name ?? "Media"}
        actions={
          inspect && (
            <>
              <button
                type="button"
                className="ds-btn ds-btn-solid"
                onClick={() => {
                  if (inspect && rename.trim() && rename.trim() !== inspect.name) {
                    void updateName(inspect.id, rename);
                  }
                }}
              >
                Save name
              </button>
              <button
                type="button"
                className="ds-btn ds-btn-danger"
                onClick={() => void offerDelete([inspect.id])}
              >
                Delete
              </button>
            </>
          )
        }
      >
        {inspect && (
          <>
            <div
              className="ds-tile-media"
              style={{ borderRadius: 10, marginBottom: 12 }}
            >
              <Thumb media={inspect} orbit={orbit} />
            </div>
            <label className="ds-field">
              <span>Name</span>
              <input
                className="ds-input"
                value={rename}
                onChange={(event) => setRename(event.target.value)}
              />
            </label>
            <p className="ds-hint">Type · {sourceLabel(inspect)}</p>
            <p className="ds-hint">
              Size ·{" "}
              {inspect.width && inspect.height
                ? `${inspect.width}×${inspect.height}`
                : "unknown"}
            </p>
            {inspect.duration_ms != null && (
              <p className="ds-hint">
                Duration · {formatDuration(inspect.duration_ms)}
              </p>
            )}
            {"size" in inspect && inspect.source === "stored" && (
              <p className="ds-hint">
                File · {(inspect.size / (1024 * 1024)).toFixed(1)} MB
              </p>
            )}
          </>
        )}
      </Inspector>

      <Confirm
        open={deleteIds != null}
        onOpenChange={(open) => {
          if (!open) {
            setDeleteIds(null);
            setDeleteUsedBy([]);
          }
        }}
        title={
          deleteIds && deleteIds.length > 1
            ? `Delete ${deleteIds.length} items?`
            : `Delete “${items.find((i) => i.id === deleteIds?.[0])?.name ?? inspect?.name ?? "this media"}”?`
        }
        description={`This cannot be undone.${usedByNote}`}
        confirmLabel="Delete"
        danger
        onConfirm={confirmDelete}
      />
    </Page>
  );
};
