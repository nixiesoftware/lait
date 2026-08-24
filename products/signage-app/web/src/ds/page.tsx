import {
  Children,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { Grid, List, Play, X } from "lucide-react";
import { space } from "@/utils/api/client";
import { ItemMenu, MoreMenu, type MenuItem } from "./menu";
import { suppressCoarseContextMenu } from "./pointer";

export function useOrbit(): string | null {
  const [orbit, setOrbit] = useState<string | null>(null);
  useEffect(() => {
    space()
      .then(setOrbit)
      .catch(() => setOrbit(null));
  }, []);
  return orbit;
}

export function Page({ children }: { children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const node = ref.current;
    if (!node) return;
    node.addEventListener("contextmenu", suppressCoarseContextMenu, true);
    return () =>
      node.removeEventListener("contextmenu", suppressCoarseContextMenu, true);
  }, []);
  return (
    <div ref={ref} className="ds-page">
      {children}
    </div>
  );
}

export function PageHeader({
  title,
  icon,
  children,
}: {
  title: string;
  icon?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <header className="ds-page-head">
      <h1 className="ds-page-title">
        {icon && <span className="ds-page-mark">{icon}</span>}
        {title}
      </h1>
      {children && <div className="ds-page-actions">{children}</div>}
    </header>
  );
}

export function PageSearch({
  value,
  onChange,
  placeholder,
}: {
  value: string;
  onChange: (value: string) => void;
  placeholder: string;
}) {
  return (
    <input
      className="ds-search"
      value={value}
      placeholder={placeholder}
      onChange={(event) => onChange(event.target.value)}
      aria-label={placeholder}
    />
  );
}

export type Chip<T extends string> = { id: T; label: string };

export function Chips<T extends string>({
  value,
  onChange,
  items,
}: {
  value: T;
  onChange: (value: T) => void;
  items: Chip<T>[];
}) {
  if (items.length === 0) return null;
  return (
    <div className="ds-chips" role="tablist">
      {items.map((chip) => (
        <button
          type="button"
          key={chip.id}
          role="tab"
          aria-selected={value === chip.id}
          className={`ds-chip${value === chip.id ? " is-on" : ""}`}
          onClick={() => onChange(chip.id)}
        >
          {chip.label}
        </button>
      ))}
    </div>
  );
}

export function Empty({
  title,
  children,
}: {
  title: string;
  children?: ReactNode;
}) {
  return (
    <div className="ds-empty-card">
      <p>{title}</p>
      {children}
    </div>
  );
}

export function SelectionBar({
  count,
  onClear,
  children,
}: {
  count: number;
  onClear: () => void;
  children?: ReactNode;
}) {
  return (
    <div className="ds-select-bar">
      <button type="button" className="ds-icon" onClick={onClear} aria-label="Clear selection">
        <X size={18} />
      </button>
      <span>
        {count} selected
      </span>
      <div className="ds-page-actions">{children}</div>
    </div>
  );
}

export function ViewToggle({
  value,
  onChange,
}: {
  value: "grid" | "list";
  onChange: (value: "grid" | "list") => void;
}) {
  return (
    <div className="ds-view-toggle" role="group" aria-label="View">
      <button
        type="button"
        className={`ds-icon${value === "grid" ? " is-on" : ""}`}
        aria-label="Grid"
        aria-pressed={value === "grid"}
        onClick={() => onChange("grid")}
      >
        <Grid size={16} />
      </button>
      <button
        type="button"
        className={`ds-icon${value === "list" ? " is-on" : ""}`}
        aria-label="List"
        aria-pressed={value === "list"}
        onClick={() => onChange("list")}
      >
        <List size={16} />
      </button>
    </div>
  );
}

export function PageStatus({
  loading,
  error,
}: {
  loading?: boolean;
  error?: string;
}) {
  if (error) return <p className="ds-danger-text">{error}</p>;
  if (loading) return <p className="ds-hint">Loading…</p>;
  return null;
}

export function CatalogueTile({
  name,
  meta,
  selected,
  onSelect,
  onOpen,
  menu,
  more,
  disabled,
  children,
}: {
  name: string;
  meta?: string;
  selected?: boolean;
  onSelect?: () => void;
  onOpen: () => void;
  menu?: MenuItem[];
  more?: MenuItem[];
  disabled?: boolean;
  children: ReactNode;
}) {
  return (
    <ItemMenu items={menu ?? []} className={`ds-tile${selected ? " is-on" : ""}`}>
      {onSelect && (
        <button
          type="button"
          className={`ds-check${selected ? " is-on" : ""}`}
          aria-label={selected ? "Deselect" : "Select"}
          aria-pressed={selected}
          disabled={disabled}
          onClick={(event) => {
            event.stopPropagation();
            onSelect();
          }}
        />
      )}
      {more && more.length > 0 && (
        <div className="ds-tile-more">
          <MoreMenu items={more} />
        </div>
      )}
      <button
        type="button"
        className="ds-tile-hit"
        disabled={disabled}
        onClick={onOpen}
      >
        <span className="ds-tile-media">{children}</span>
        <span className="ds-tile-name">{name}</span>
        {meta && <span className="ds-tile-meta">{meta}</span>}
      </button>
    </ItemMenu>
  );
}

export function CatalogueRow({
  name,
  meta,
  selected,
  onSelect,
  onOpen,
  menu,
  more,
  disabled,
  children,
}: {
  name: string;
  meta?: string;
  selected?: boolean;
  onSelect?: () => void;
  onOpen: () => void;
  menu?: MenuItem[];
  more?: MenuItem[];
  disabled?: boolean;
  children?: ReactNode;
}) {
  return (
    <ItemMenu items={menu ?? []} className={`ds-row${selected ? " is-on" : ""}`}>
      {onSelect && (
        <button
          type="button"
          className={`ds-check ds-check-inline${selected ? " is-on" : ""}`}
          aria-label={selected ? "Deselect" : "Select"}
          aria-pressed={selected}
          disabled={disabled}
          onClick={(event) => {
            event.stopPropagation();
            onSelect();
          }}
        />
      )}
      {children && <span className="ds-row-media">{children}</span>}
      <button
        type="button"
        className="ds-row-hit"
        disabled={disabled}
        onClick={onOpen}
      >
        <span className="ds-row-copy">
          <strong>{name}</strong>
          {meta && <span>{meta}</span>}
        </span>
      </button>
      {more && more.length > 0 && <MoreMenu items={more} />}
    </ItemMenu>
  );
}

/** Photos / Frame.io: the image is the cell. Caption sits on it. */
export function GalleryShot({
  name,
  badge,
  play,
  selected,
  onSelect,
  onOpen,
  menu,
  more,
  disabled,
  children,
}: {
  name: string;
  badge?: string;
  play?: boolean;
  selected?: boolean;
  onSelect?: () => void;
  onOpen: () => void;
  menu?: MenuItem[];
  more?: MenuItem[];
  disabled?: boolean;
  children: ReactNode;
}) {
  return (
    <ItemMenu items={menu ?? []} className={`ds-shot${selected ? " is-on" : ""}`}>
      {onSelect && (
        <button
          type="button"
          className={`ds-check${selected ? " is-on" : ""}`}
          aria-label={selected ? "Deselect" : "Select"}
          aria-pressed={selected}
          disabled={disabled}
          onClick={(event) => {
            event.stopPropagation();
            onSelect();
          }}
        />
      )}
      {more && more.length > 0 && (
        <div className="ds-tile-more">
          <MoreMenu items={more} />
        </div>
      )}
      <button
        type="button"
        className="ds-shot-hit"
        disabled={disabled}
        onClick={onOpen}
      >
        <span className="ds-shot-media">{children}</span>
        {play && (
          <span className="ds-shot-play" aria-hidden="true">
            <Play size={14} fill="currentColor" />
          </span>
        )}
        <span className="ds-shot-cap">
          <span className="ds-shot-name">{name}</span>
          {badge && <span className="ds-shot-badge">{badge}</span>}
        </span>
      </button>
    </ItemMenu>
  );
}

/** Spotify library: square collage + title + counts. */
export function Cover({ children }: { children: ReactNode }) {
  const cells = Children.toArray(children);
  const single = cells.length <= 1;
  return (
    <span className={`ds-cover${single ? " is-1" : ""}`}>
      {(single ? cells.slice(0, 1) : cells.slice(0, 4)).map((cell, i) => (
        <span key={i} className="ds-cover-cell">
          {cell}
        </span>
      ))}
    </span>
  );
}

export function PlaylistRow({
  name,
  meta,
  selected,
  onSelect,
  onOpen,
  menu,
  more,
  children,
}: {
  name: string;
  meta?: string;
  selected?: boolean;
  onSelect?: () => void;
  onOpen: () => void;
  menu?: MenuItem[];
  more?: MenuItem[];
  children?: ReactNode;
}) {
  return (
    <ItemMenu items={menu ?? []} className={`ds-pl${selected ? " is-on" : ""}`}>
      {onSelect && (
        <button
          type="button"
          className={`ds-check ds-check-inline${selected ? " is-on" : ""}`}
          aria-label={selected ? "Deselect" : "Select"}
          aria-pressed={selected}
          onClick={(event) => {
            event.stopPropagation();
            onSelect();
          }}
        />
      )}
      {children}
      <button type="button" className="ds-row-hit" onClick={onOpen}>
        <span className="ds-row-copy">
          <strong>{name}</strong>
          {meta && <span>{meta}</span>}
        </span>
      </button>
      {more && more.length > 0 && <MoreMenu items={more} />}
    </ItemMenu>
  );
}

/** Tailscale / Revolut devices: a monitor, then the facts. */
export function PlaylistTile({
  name,
  meta,
  selected,
  onSelect,
  onOpen,
  menu,
  more,
  children,
}: {
  name: string;
  meta?: string;
  selected?: boolean;
  onSelect?: () => void;
  onOpen: () => void;
  menu?: MenuItem[];
  more?: MenuItem[];
  children?: ReactNode;
}) {
  return (
    <ItemMenu items={menu ?? []} className={`ds-pl-tile${selected ? " is-on" : ""}`}>
      {onSelect && (
        <button
          type="button"
          className={`ds-check${selected ? " is-on" : ""}`}
          aria-label={selected ? "Deselect" : "Select"}
          aria-pressed={selected}
          onClick={(event) => {
            event.stopPropagation();
            onSelect();
          }}
        />
      )}
      {more && more.length > 0 && (
        <div className="ds-tile-more">
          <MoreMenu items={more} />
        </div>
      )}
      <button type="button" className="ds-tile-hit" onClick={onOpen}>
        {children}
        <span className="ds-row-copy">
          <strong>{name}</strong>
          {meta && <span>{meta}</span>}
        </span>
      </button>
    </ItemMenu>
  );
}

export function DeviceRow({
  name,
  meta,
  onOpen,
  menu,
  more,
  children,
}: {
  name: string;
  meta?: string;
  onOpen: () => void;
  menu?: MenuItem[];
  more?: MenuItem[];
  children?: ReactNode;
}) {
  return (
    <ItemMenu items={menu ?? []} className="ds-device">
      <span className="ds-bezel">{children}</span>
      <button type="button" className="ds-row-hit" onClick={onOpen}>
        <span className="ds-row-copy">
          <strong>{name}</strong>
          {meta && <span>{meta}</span>}
        </span>
      </button>
      {more && more.length > 0 && <MoreMenu items={more} />}
    </ItemMenu>
  );
}
