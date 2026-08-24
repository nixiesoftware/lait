import type { ReactNode, RefObject } from "react";
import { ContextMenu } from "@base-ui/react/context-menu";
import type { ClipCopy, LaidClip } from "./model";
import { useCoarsePointer } from "./pointer";

export type MenuItem = {
  label: string;
  disabled?: boolean;
  danger?: boolean;
  onPick: () => void;
};

export type ClipActions = {
  clipboard: ClipCopy | null;
  duplicate: (id: string) => void;
  copy: (id: string) => void;
  pasteAfter: (id: string | null) => void;
  remove: (id: string) => void;
  add: () => void;
};

export function clipMenuItems(clip: LaidClip, actions: ClipActions): MenuItem[] {
  return [
    { label: "Duplicate", onPick: () => actions.duplicate(clip.item.id) },
    { label: "Copy", onPick: () => actions.copy(clip.item.id) },
    {
      label: "Paste after",
      disabled: !actions.clipboard,
      onPick: () => actions.pasteAfter(clip.item.id),
    },
    {
      label: "Remove",
      danger: true,
      onPick: () => actions.remove(clip.item.id),
    },
  ];
}

export function trackMenuItems(actions: ClipActions): MenuItem[] {
  return [
    { label: "Add media", onPick: actions.add },
    {
      label: "Paste",
      disabled: !actions.clipboard,
      onPick: () => actions.pasteAfter(null),
    },
  ];
}

export function OverlayMenu({
  items,
  container,
}: {
  items: MenuItem[];
  container?: RefObject<HTMLElement | null>;
}) {
  return (
    <ContextMenu.Portal container={container}>
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

export function ItemMenu({
  items,
  container,
  className,
  style,
  children,
}: {
  items: MenuItem[];
  container?: RefObject<HTMLElement | null>;
  className?: string;
  style?: React.CSSProperties;
  children: ReactNode;
}) {
  const coarse = useCoarsePointer();
  if (coarse) {
    return (
      <div className={className} style={style}>
        {children}
      </div>
    );
  }
  return (
    <ContextMenu.Root>
      <ContextMenu.Trigger className={className} style={style}>
        {children}
      </ContextMenu.Trigger>
      <OverlayMenu items={items} container={container} />
    </ContextMenu.Root>
  );
}
