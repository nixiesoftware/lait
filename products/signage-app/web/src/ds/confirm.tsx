import { useState } from "react";
import { AlertDialog } from "@base-ui/react/alert-dialog";

type Props = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description: string;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
  onConfirm: () => void | Promise<void>;
};

export function Confirm({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel = "Confirm",
  cancelLabel = "Cancel",
  danger,
  onConfirm,
}: Props) {
  const [busy, setBusy] = useState(false);

  const run = async () => {
    if (busy) return;
    setBusy(true);
    try {
      await onConfirm();
      onOpenChange(false);
    } finally {
      setBusy(false);
    }
  };

  return (
    <AlertDialog.Root open={open} onOpenChange={onOpenChange}>
      <AlertDialog.Portal>
        <AlertDialog.Backdrop className="ds-backdrop" />
        <AlertDialog.Popup className="ds-dialog ds-leave">
          <AlertDialog.Title>{title}</AlertDialog.Title>
          <AlertDialog.Description>{description}</AlertDialog.Description>
          <menu>
            <AlertDialog.Close className="ds-btn ds-btn-quiet" disabled={busy}>
              {cancelLabel}
            </AlertDialog.Close>
            <button
              type="button"
              className={`ds-btn ${danger ? "ds-btn-danger" : "ds-btn-solid"}`}
              disabled={busy}
              onClick={run}
            >
              {busy ? "Working…" : confirmLabel}
            </button>
          </menu>
        </AlertDialog.Popup>
      </AlertDialog.Portal>
    </AlertDialog.Root>
  );
}
