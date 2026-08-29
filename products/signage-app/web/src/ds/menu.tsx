import type { ReactNode } from "react";
import { ContextMenu } from "@base-ui/react/context-menu";
import { Menu } from "@base-ui/react/menu";
import { Ellipsis } from "lucide-react";
import { useCoarsePointer } from "./pointer";

export type MenuItem = {
  label: string;
  disabled?: boolean;
  danger?: boolean;
  onPick: () => void;
};

export function OverlayMenu({ items }: { items: MenuItem[] }) {
  return (
    <ContextMenu.Portal>
      <ContextMenu.Positioner
        className="ds-overlay"
        align="start"
        side="bottom"
        sideOffset={4}
      >
        <ContextMenu.Popup className="ds-menu">
          {items.map((item) => (
            <ContextMenu.Item
              key={item.label}
              className={`ds-menu-item${item.danger ? " is-danger" : ""}`}
              disabled={item.disabled}
              onClick={() => {
                if (item.disabled) return;
                item.onPick();
              }}
            >
              {item.label}
            </ContextMenu.Item>
          ))}
        </ContextMenu.Popup>
      </ContextMenu.Positioner>
    </ContextMenu.Portal>
  );
}

/** Right-click on a mouse. Fingers never see this — inspector / MoreMenu is theirs. */
export function ItemMenu({
  items,
  className,
  children,
}: {
  items: MenuItem[];
  className?: string;
  children: ReactNode;
}) {
  const coarse = useCoarsePointer();
  if (coarse || items.length === 0) {
    return <div className={className}>{children}</div>;
  }
  return (
    <ContextMenu.Root>
      <ContextMenu.Trigger className={className}>{children}</ContextMenu.Trigger>
      <OverlayMenu items={items} />
    </ContextMenu.Root>
  );
}

export type ChoiceItem = {
  id: string;
  label: string;
  hint?: string;
  on?: boolean;
  disabled?: boolean;
  danger?: boolean;
};

/**
 * A value you change by choosing: the control shows what is chosen, opens on
 * one press, and the pick is the commit. Two gestures, and the second one is
 * the outcome — there is no field to fill and nothing to submit afterwards.
 */
export function ChoiceMenu({
  label,
  items,
  onPick,
  className,
  align = "start",
  children,
}: {
  label: string;
  items: ChoiceItem[];
  onPick: (id: string) => void;
  className?: string;
  align?: "start" | "end";
  children: ReactNode;
}) {
  return (
    <Menu.Root>
      <Menu.Trigger
        className={className}
        aria-label={label}
        onClick={(event) => event.stopPropagation()}
      >
        {children}
      </Menu.Trigger>
      <Menu.Portal>
        <Menu.Positioner className="ds-overlay" sideOffset={4} align={align}>
          <Menu.Popup className="ds-menu ds-menu-choice">
            {items.length === 0 && <span className="ds-menu-empty">Nothing to choose from yet.</span>}
            {items.map((item) => (
              <Menu.Item
                key={item.id}
                className={`ds-menu-item${item.on ? " is-on" : ""}${item.danger ? " is-danger" : ""}`}
                disabled={item.disabled}
                onClick={() => {
                  if (item.disabled) return;
                  onPick(item.id);
                }}
              >
                <span className="ds-menu-check" aria-hidden />
                <span className="ds-menu-copy">
                  {item.label}
                  {item.hint && <small>{item.hint}</small>}
                </span>
              </Menu.Item>
            ))}
          </Menu.Popup>
        </Menu.Positioner>
      </Menu.Portal>
    </Menu.Root>
  );
}

/** Always-visible ⋯. Works on coarse and fine. */
export function MoreMenu({
  items,
  label = "More",
}: {
  items: MenuItem[];
  label?: string;
}) {
  if (items.length === 0) return null;
  return (
    <Menu.Root>
      <Menu.Trigger
        className="ds-icon ds-more"
        aria-label={label}
        onClick={(event) => event.stopPropagation()}
      >
        <Ellipsis size={18} />
      </Menu.Trigger>
      <Menu.Portal>
        <Menu.Positioner className="ds-overlay" sideOffset={4} align="end">
          <Menu.Popup className="ds-menu">
            {items.map((item) => (
              <Menu.Item
                key={item.label}
                className={`ds-menu-item${item.danger ? " is-danger" : ""}`}
                disabled={item.disabled}
                onClick={() => {
                  if (item.disabled) return;
                  item.onPick();
                }}
              >
                {item.label}
              </Menu.Item>
            ))}
          </Menu.Popup>
        </Menu.Positioner>
      </Menu.Portal>
    </Menu.Root>
  );
}
