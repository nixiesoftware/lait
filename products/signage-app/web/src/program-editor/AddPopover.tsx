import { useRef, useState, type RefObject } from "react";
import { motion } from "framer-motion";
import { Popover } from "@base-ui/react/popover";
import {
  AppWindow,
  ArrowLeft,
  Library,
  Plus,
  Upload,
  X,
} from "lucide-react";
import { overlayTransition } from "@/ds";
import type { SignageMedia } from "@/utils/lait/types";
import { KIND_PANELS, type KindPanel } from "./kinds/registry";
import { uploadContentAll } from "@/utils/content/api";
import { LibraryPicker } from "./LibraryPicker";

const chooseList = {
  hidden: {},
  show: { transition: { staggerChildren: 0.06, delayChildren: 0.04 } },
};

const chooseTile = {
  hidden: { opacity: 0, y: 8 },
  show: { opacity: 1, y: 0, transition: overlayTransition },
};

export type AddPage = "choose" | "library" | "apps";

type FlowProps = {
  page: AddPage;
  onPage: (page: AddPage) => void;
  library: SignageMedia[];
  orbit: string | null;
  variant: "grid" | "list";
  onAdd: (media: SignageMedia) => void;
  onUploaded: (media: SignageMedia[]) => void;
  onAddKind: (kind: KindPanel) => void;
  onUploadError?: (message: string) => void;
};

export function AddFlow({
  page,
  onPage,
  library,
  orbit,
  variant,
  onAdd,
  onUploaded,
  onAddKind,
  onUploadError,
}: FlowProps) {
  const inputRef = useRef<HTMLInputElement>(null);

  const upload = async (files: FileList | null) => {
    if (!files || files.length === 0) return;
    try {
      const outcome = await uploadContentAll([...files]);
      onUploaded(outcome.uploaded);
      if (outcome.refused.length > 0) {
        onUploadError?.(outcome.refused.map((row) => row.reason).join(" "));
      }
    } catch (err) {
      onUploadError?.(err instanceof Error ? err.message : String(err));
    }
  };

  if (page === "choose") {
    return (
      <>
        <motion.div
          className="ds-choose"
          initial="hidden"
          animate="show"
          variants={chooseList}
        >
          <motion.button
            type="button"
            className="ds-choose-tile"
            variants={chooseTile}
            onClick={() => onPage("library")}
          >
            <Library strokeWidth={1.75} />
            Library
          </motion.button>
          <motion.button
            type="button"
            className="ds-choose-tile"
            variants={chooseTile}
            onClick={() => inputRef.current?.click()}
          >
            <Upload strokeWidth={1.75} />
            Upload
          </motion.button>
          <motion.button
            type="button"
            className="ds-choose-tile"
            variants={chooseTile}
            onClick={() => onPage("apps")}
          >
            <AppWindow strokeWidth={1.75} />
            Apps
          </motion.button>
        </motion.div>
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
      </>
    );
  }

  if (page === "apps") {
    return (
      <div className="ds-sheet-body">
        {KIND_PANELS.map((kind) => (
          <button
            type="button"
            key={kind.kind}
            className="ds-row"
            onClick={() => onAddKind(kind)}
          >
            <span className="ds-row-copy">
              {kind.label}
              <span>{kind.description}</span>
            </span>
          </button>
        ))}
      </div>
    );
  }

  return (
    <LibraryPicker
      variant={variant}
      library={library}
      orbit={orbit}
      onAdd={onAdd}
      onUploaded={onUploaded}
      onUploadError={onUploadError}
    />
  );
}

type Props = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  library: SignageMedia[];
  orbit: string | null;
  onAdd: (media: SignageMedia) => void;
  onUploaded: (media: SignageMedia[]) => void;
  onAddKind: (kind: KindPanel) => void;
  onUploadError?: (message: string) => void;
  container?: RefObject<HTMLElement | null>;
  asButton?: boolean;
  style?: React.CSSProperties;
};

export function AddPopover({
  open,
  onOpenChange,
  library,
  orbit,
  onAdd,
  onUploaded,
  onAddKind,
  onUploadError,
  container,
  asButton,
  style,
}: Props) {
  const [page, setPage] = useState<AddPage>("choose");

  const setOpen = (next: boolean) => {
    if (!next) setPage("choose");
    onOpenChange(next);
  };

  const title =
    page === "library" ? "Library" : page === "apps" ? "Apps" : "Add";

  if (asButton) {
    return (
      <button
        type="button"
        className="pe-add"
        style={style}
        aria-label="Add media"
        onClick={() => setOpen(true)}
      >
        <Plus size={28} strokeWidth={2} />
      </button>
    );
  }

  return (
    <Popover.Root open={open} onOpenChange={setOpen}>
      <Popover.Trigger className="pe-add" style={style} aria-label="Add media">
        <Plus size={28} strokeWidth={2} />
      </Popover.Trigger>
      <Popover.Portal container={container}>
        <Popover.Positioner
          className="ds-overlay"
          side="top"
          align="center"
          sideOffset={8}
        >
          <Popover.Popup
            className={`ds-pop${page === "choose" ? " ds-pop-choose" : " ds-pop-wide"}`}
            aria-label="Add to program"
          >
            {page === "choose" ? (
              <Popover.Title className="ds-sr">Add to program</Popover.Title>
            ) : (
              <header className="ds-pop-head">
                <button
                  type="button"
                  className="ds-icon"
                  aria-label="Back"
                  onClick={() => setPage("choose")}
                >
                  <ArrowLeft size={16} />
                </button>
                <Popover.Title>{title}</Popover.Title>
                <Popover.Close className="ds-icon" aria-label="Close">
                  <X size={16} />
                </Popover.Close>
              </header>
            )}
            <AddFlow
              page={page}
              onPage={setPage}
              library={library}
              orbit={orbit}
              variant="grid"
              onAdd={onAdd}
              onUploaded={onUploaded}
              onAddKind={onAddKind}
              onUploadError={onUploadError}
            />
          </Popover.Popup>
        </Popover.Positioner>
      </Popover.Portal>
    </Popover.Root>
  );
}
