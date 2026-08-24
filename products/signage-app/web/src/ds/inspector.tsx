import type { ReactNode } from "react";
import { Dialog } from "@base-ui/react/dialog";
import { X } from "lucide-react";

export function Inspector({
  open,
  onOpenChange,
  title,
  mark,
  kicker,
  className,
  children,
  actions,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  mark?: ReactNode;
  kicker?: ReactNode;
  className?: string;
  children: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Backdrop className="ds-backdrop" />
        <Dialog.Popup
          className={`ds-sheet ds-inspector${className ? ` ${className}` : ""}`}
          aria-label={title}
        >
          <div className="ds-sheet-grab" aria-hidden="true">
            <i />
          </div>
          <div className="ds-sheet-head">
            <div className="ds-sheet-ident">
              {mark}
              <div>
                <h2>{title}</h2>
                {kicker}
              </div>
            </div>
            <Dialog.Close className="ds-icon" aria-label="Close">
              <X size={18} />
            </Dialog.Close>
          </div>
          <div className="ds-sheet-body">{children}</div>
          {actions && <div className="ds-inspector-actions">{actions}</div>}
        </Dialog.Popup>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

export type PickItem = {
  id: string;
  label: string;
  hint?: string;
  disabled?: boolean;
  danger?: boolean;
};

export function Picker({
  open,
  onOpenChange,
  title,
  items,
  empty,
  onPick,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  items: PickItem[];
  empty?: string;
  onPick: (id: string) => void;
}) {
  return (
    <Inspector open={open} onOpenChange={onOpenChange} title={title}>
      {items.length === 0 && <p className="ds-hint">{empty ?? "Nothing here."}</p>}
      {items.map((item) => (
        <button
          type="button"
          key={item.id}
          className={`ds-row${item.danger ? " is-danger" : ""}`}
          disabled={item.disabled}
          onClick={() => {
            if (item.disabled) return;
            onPick(item.id);
            onOpenChange(false);
          }}
        >
          <span className="ds-row-copy">
            <strong>{item.label}</strong>
            {item.hint && <span>{item.hint}</span>}
          </span>
        </button>
      ))}
    </Inspector>
  );
}
