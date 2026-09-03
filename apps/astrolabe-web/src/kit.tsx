/**
 * The canonical presentation pieces every Astrolabe surface shares — buttons,
 * fields, cards, chips, the person row, fact rows, the disclosure, and the
 * app dialog. One vocabulary, drawn once: react-aria carries the behavior
 * (focus, keyboard, dismissal) and `styles.css` carries the whole look, so
 * no surface styles a control of its own or imports a component library.
 */
import {
  Button as AriaButton,
  Dialog as AriaDialog,
  Heading,
  Menu,
  MenuItem,
  MenuTrigger,
  Modal,
  ModalOverlay,
  Popover,
} from "react-aria-components";
import type { ReactNode } from "react";

import { IconMore, IconUser } from "./icons";

export type Presence = "online" | "away" | "offline" | null;

/**
 * Wording for a measured presence, and nothing for an absence: an unmeasured
 * presence has no words because it is not a fact about the peer.
 */
export function presenceLabel(presence: Presence): string | null {
  switch (presence) {
    case "online": return "Online";
    case "away": return "Away";
    case "offline": return "Offline";
    case null: return null;
  }
}

/** The stored `<mime>;base64,<data>` form, resolved to a drawable URI. */
export function pictureUri(stored: string | null): string | null {
  if (stored === null || !stored.includes(";base64,")) return null;
  return `data:${stored}`;
}

/**
 * The stored picture when one was authored, else the default — a monogram,
 * or the person mark when there is nothing to monogram.
 */
export function FacePlate({ picture, name, size, agent = false }: {
  picture: string | null; name: string; size: number; agent?: boolean;
}) {
  const uri = pictureUri(picture);
  return <span className="face-plate" data-agent={agent || undefined}
    style={{ width: size, height: size, fontSize: size * 0.36 }} aria-hidden>
    {uri !== null ? <img src={uri} alt="" />
      : name === "" ? <IconUser size={Math.round(size * 0.5)} /> : name.slice(0, 1).toUpperCase()}
  </span>;
}

/** The shipped AI mark an agent's row wears beside its name. */
export function AiMark() {
  return <span className="ai-mark" title="An agent" aria-label="Agent">AI</span>;
}

/** A badge is one word or two and never breaks: a label folded onto two lines reads as two badges. */
export function Badge({ label, solid = false }: { label: string; solid?: boolean }) {
  return <span className={solid ? "badge badge-solid" : "badge"}>{label}</span>;
}

/** A tinted standing chip. Neutral states nothing; the other tones grade it. */
export function Chip({ label, tone = "neutral" }: { label: string; tone?: "good" | "neutral" | "warn" | "crit" }) {
  return <span className="chip" data-tone={tone}>{label}</span>;
}

/**
 * The one button. `primary` commits, `quiet` sits beside it, `danger` costs
 * something, `text` reads as a link, `ghost` is chrome, `icon` is a glyph.
 */
export function Button({ variant = "quiet", onPress, disabled = false, label, children }: {
  variant?: "primary" | "quiet" | "danger" | "text" | "ghost" | "icon";
  onPress(): void; disabled?: boolean; label?: string; children: ReactNode;
}) {
  return <AriaButton className={`${variant}-button`} onPress={onPress} isDisabled={disabled} aria-label={label}>
    {children}
  </AriaButton>;
}

/**
 * A labelled field. The input is the caller's — the label wraps it so the
 * association is the markup, and an error is a line under it, never a color
 * alone.
 */
export function Field({ label, error, children }: { label: string; error?: string; children: ReactNode }) {
  return <label className="field">
    <span>{label}</span>
    {children}
    {error !== undefined && <span className="field-error">{error}</span>}
  </label>;
}

/** A bordered card of facts and controls — the row container of every list. */
export function Card({ children }: { children: ReactNode }) {
  return <div className="card">{children}</div>;
}

/**
 * A quiet fold at the foot of a surface, for what the daemon knows that a
 * person does not need. Native `details`, because that is exactly what it is.
 */
export function Disclosure({ summary, children }: { summary: ReactNode; children: ReactNode }) {
  return <details className="disclosure">
    <summary>{summary}</summary>
    <div className="disclosure-panel">{children}</div>
  </details>;
}

/** A row's overflow menu: a glyph, and the few acts that did not earn a button. */
export function RowMenu({ label = "More", items }: {
  label?: string;
  items: { label: string; onAction(): void; disabled?: boolean; danger?: boolean }[];
}) {
  return <MenuTrigger>
    <AriaButton className="icon-button" aria-label={label}><IconMore /></AriaButton>
    <Popover className="menu-popover" placement="bottom end">
      <Menu className="menu-list">
        {items.map((item) => <MenuItem key={item.label} className={item.danger ? "danger-item" : ""}
          isDisabled={item.disabled} onAction={item.onAction}>{item.label}</MenuItem>)}
      </Menu>
    </Popover>
  </MenuTrigger>;
}

/**
 * The canonical person row — the face, the name with the AI mark when the
 * identity is an agent, and beneath them a line that is only ever a fact:
 * measured presence when a Space that names them answered, else an authored
 * note, else nothing. Liveness reads through weight: an offline face dims
 * hardest, an away face part-way.
 */
export function PersonTile({ name, picture, presence, agent = false, note, size = 40, trailing }: {
  name: string; picture: string | null; presence: Presence; agent?: boolean; note?: string; size?: number; trailing?: ReactNode;
}) {
  const status = presenceLabel(presence) ?? (note !== undefined && note !== "" ? note : null);
  const dim = presence === "offline" ? 0.45 : presence === "away" ? 0.7 : 1;
  return <span className="person-tile">
    <span style={{ opacity: dim, display: "inline-flex" }}><FacePlate picture={picture} name={name} size={size} agent={agent} /></span>
    <span className="person-copy">
      <span className="person-name" data-offline={presence === "offline" || undefined}>
        <strong>{name}</strong>
        {agent && <AiMark />}
      </span>
      {status !== null && <small data-online={presence === "online" || undefined}>{status}</small>}
    </span>
    {trailing}
  </span>;
}

/**
 * The head of a full-product pane: the name, and at most one act. The OS
 * draws the window's title bar and the rail carries the navigation, so this
 * is the only header the pane itself adds. No prose — orientation belongs to
 * the list and its empty state.
 */
export function PaneHead({ title, action }: { title: string; action?: ReactNode }) {
  return <div className="pane-head">
    <h1>{title}</h1>
    {action}
  </div>;
}

/** A labelled fact row whose value is selectable, never truncated by hand. */
export function Fact({ label, value }: { label: string; value: string }) {
  return <div className="fact-row">
    <span className="fact-label">{label}</span>
    <span className="fact-value">{value}</span>
  </div>;
}

/** A section marking: an emphatic label with the count dim beside it. */
export function SectionTitle({ label, count }: { label: string; count?: number }) {
  return <div className="section-title">
    <span>{label}</span>
    {count !== undefined && <span>{count}</span>}
  </div>;
}

export function Empty({ said, next, icon, action }: { said: string; next?: string; icon?: ReactNode; action?: ReactNode }) {
  return <div className="empty-state">
    {icon !== undefined && <span className="empty-icon">{icon}</span>}
    <strong>{said}</strong>
    {next !== undefined && <p>{next}</p>}
    {action !== undefined && <span className="empty-action">{action}</span>}
  </div>;
}

export function Notice({ tone, children }: { tone: "warn" | "danger" | "good"; children: ReactNode }) {
  return <div className={`notice notice-${tone}`} role={tone === "danger" ? "alert" : "status"}>{children}</div>;
}

/**
 * The app dialog. Rendered by the surface that owns the draft, dismissed by
 * Escape, the scrim, or its own controls; focus lands inside on mount. Kept
 * deliberately small — drafts live in the caller, facts stay in the view.
 */
export function AppDialog({ title, description, onDismiss, children }: {
  title: string; description?: string; onDismiss(): void; children: ReactNode;
}) {
  return <ModalOverlay className="dialog-overlay" isOpen isDismissable
    onOpenChange={(open) => { if (!open) onDismiss(); }}>
    <Modal className="dialog-modal">
      <AriaDialog className="dialog-body">
        <header>
          <Heading level={2} slot="title">{title}</Heading>
          {description !== undefined && <p>{description}</p>}
        </header>
        <div className="dialog-fields">{children}</div>
      </AriaDialog>
    </Modal>
  </ModalOverlay>;
}

export function DialogFooter({ children }: { children: ReactNode }) {
  return <footer className="dialog-footer">{children}</footer>;
}

export function shortId(value: string): string {
  return value.length <= 12 ? value : `${value.slice(0, 12)}…`;
}

/** `snake_case` wire words, presented capitalised. */
export function words(value: string): string {
  return value
    .split("_")
    .filter((part) => part !== "")
    .map((part) => `${part[0].toUpperCase()}${part.slice(1)}`)
    .join(" ");
}
