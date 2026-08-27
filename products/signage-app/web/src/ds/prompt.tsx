import { useEffect, useState } from "react";
import { Dialog } from "@base-ui/react/dialog";

type Props = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: string;
  label?: string;
  placeholder?: string;
  confirmLabel?: string;
  initial?: string;
  busy?: boolean;
  onSubmit: (value: string) => void | Promise<void>;
};

export function Prompt({
  open,
  onOpenChange,
  title,
  description,
  label,
  placeholder,
  confirmLabel = "Save",
  initial = "",
  busy,
  onSubmit,
}: Props) {
  const [value, setValue] = useState(initial);
  const [error, setError] = useState("");
  const [working, setWorking] = useState(false);

  useEffect(() => {
    if (open) {
      setValue(initial);
      setError("");
    }
  }, [open, initial]);

  const run = async () => {
    const next = value.trim();
    if (!next) {
      setError("A name is required");
      return;
    }
    setWorking(true);
    try {
      await onSubmit(next);
      onOpenChange(false);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setWorking(false);
    }
  };

  const pending = busy || working;

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Backdrop className="ds-backdrop" />
        <Dialog.Popup className="ds-dialog ds-leave">
          <Dialog.Title>{title}</Dialog.Title>
          {description && (
            <Dialog.Description>{description}</Dialog.Description>
          )}
          <label className="ds-field">
            {label && <span>{label}</span>}
            <input
              className="ds-input"
              value={value}
              placeholder={placeholder}
              autoFocus
              disabled={pending}
              onChange={(event) => {
                setValue(event.target.value);
                setError("");
              }}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  void run();
                }
              }}
            />
          </label>
          {error && <p className="ds-danger-text">{error}</p>}
          <menu>
            <Dialog.Close className="ds-btn ds-btn-quiet" disabled={pending}>
              Cancel
            </Dialog.Close>
            <button
              type="button"
              className="ds-btn ds-btn-solid"
              disabled={pending}
              onClick={() => void run()}
            >
              {pending ? "Working…" : confirmLabel}
            </button>
          </menu>
        </Dialog.Popup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
