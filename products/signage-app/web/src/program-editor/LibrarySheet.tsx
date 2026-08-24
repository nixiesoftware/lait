import { useRef, useState, type RefObject } from "react";
import { Dialog } from "@base-ui/react/dialog";
import { useDrag } from "@use-gesture/react";
import { ArrowLeft, X } from "lucide-react";
import type { SignageMedia } from "@/utils/lait/types";
import type { KindDefinition } from "@/utils/apps/api";
import { AddFlow, type AddPage } from "./AddPopover";

type Props = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  library: SignageMedia[];
  orbit: string | null;
  onAdd: (media: SignageMedia) => void;
  onUploaded: (media: SignageMedia[]) => void;
  onAddKind: (kind: KindDefinition) => void;
  onUploadError?: (message: string) => void;
  container?: RefObject<HTMLElement | null>;
};

export function LibrarySheet({
  open,
  onOpenChange,
  library,
  orbit,
  onAdd,
  onUploaded,
  onAddKind,
  onUploadError,
  container,
}: Props) {
  const [page, setPage] = useState<AddPage>("choose");
  const [dy, setDy] = useState(0);
  const dragging = useRef(false);
  const title =
    page === "library" ? "Library" : page === "apps" ? "Apps" : "Add";

  const bindGrab = useDrag(
    ({ down, movement: [, my], last, cancel }) => {
      if (page !== "choose" && my < 0) return;
      dragging.current = down;
      if (down) {
        setDy(Math.max(0, my));
        return;
      }
      if (last) {
        if (my > 88) {
          setDy(0);
          setPage("choose");
          onOpenChange(false);
          return;
        }
        setDy(0);
        cancel?.();
      }
    },
    { axis: "y", filterTaps: true, pointer: { touch: true } },
  );

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(next) => {
        if (!next) {
          setPage("choose");
          setDy(0);
        }
        onOpenChange(next);
      }}
    >
      <Dialog.Portal container={container}>
        <Dialog.Backdrop className="ds-backdrop" />
        <Dialog.Popup
          className="ds-sheet"
          aria-label="Add to program"
          style={
            dy
              ? { transform: `translateY(${dy}px)`, transition: "none" }
              : undefined
          }
        >
          <div className="ds-sheet-grab" {...bindGrab()} aria-hidden>
            <i />
          </div>
          <header className="ds-sheet-head">
            {page === "choose" ? (
              <Dialog.Title>{title}</Dialog.Title>
            ) : (
              <>
                <button
                  type="button"
                  className="ds-icon"
                  aria-label="Back"
                  onClick={() => setPage("choose")}
                >
                  <ArrowLeft size={18} />
                </button>
                <Dialog.Title>{title}</Dialog.Title>
              </>
            )}
            <Dialog.Close className="ds-icon" aria-label="Close">
              <X size={18} />
            </Dialog.Close>
          </header>
          <div className="ds-sheet-body">
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
          </div>
        </Dialog.Popup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
