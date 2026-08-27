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
