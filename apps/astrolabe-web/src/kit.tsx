/**
 * The canonical presentation pieces every Astrolabe surface shares — the face
 * on a card, the person row, badges, fact rows, and the app dialog. These are
 * the web spellings of the Flutter client's `face.dart` / `person.dart`
 * anatomy: a tile draws facts and nothing else; a surface composes gestures
 * and controls around it.
 */
import { useEffect, useRef, type ReactNode } from "react";

import type { Card } from "./client";

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

/**
 * The canonical group the daemon files an agent's card under — part of the
 * book's wire vocabulary, not a display string.
 */
const agentGroup = "Agents";

/**
 * An agent's own card: filed under the agent group, or carrying nothing but
 * `agent:` spellings. Worn as the AI mark on a row — never as a section: what
 * an identity is and whether it is here are different axes.
 */
export function isAgentCard(card: Pick<Card, "groups" | "agents" | "addresses" | "devices">): boolean {
  return card.groups.includes(agentGroup)
    || (card.agents.length > 0 && card.addresses.length === 0 && card.devices.length === 0);
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
export function FacePlate({ picture, name, size }: { picture: string | null; name: string; size: number }) {
  const uri = pictureUri(picture);
  return <span className="face-plate" style={{ width: size, height: size, fontSize: size * 0.36 }} aria-hidden>
    {uri !== null ? <img src={uri} alt="" /> : name === "" ? "◌" : name.slice(0, 1).toUpperCase()}
  </span>;
}

/** The shipped AI mark an agent's row wears beside its name. */
export function AiMark() {
  return <span className="ai-mark" title="An agent" aria-label="Agent">AI</span>;
}

export function Badge({ label, solid = false }: { label: string; solid?: boolean }) {
  return <span className={solid ? "badge badge-solid" : "badge"}>{label}</span>;
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
    <span style={{ opacity: dim, display: "inline-flex" }}><FacePlate picture={picture} name={name} size={size} /></span>
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

/** A labelled fact row whose value is selectable, never truncated by hand. */
export function Fact({ label, value }: { label: string; value: string }) {
  return <div className="fact-row"><span className="fact-label">{label}</span><code className="fact-value">{value}</code></div>;
}

/** A section marking: an emphatic label with the count dim beside it. */
export function SectionTitle({ label, count }: { label: string; count?: number }) {
  return <div className="section-heading section-title"><span>{label}</span>{count !== undefined && <span>{count}</span>}</div>;
}

export function Empty({ said, next }: { said: string; next?: string }) {
  return <div className="empty-state"><strong>{said}</strong>{next !== undefined && <p>{next}</p>}</div>;
}

export function Notice({ tone, children }: { tone: "warn" | "danger" | "good"; children: ReactNode }) {
  return <p className={`notice notice-${tone}`}>{children}</p>;
}

/**
 * The app dialog. Rendered by the surface that owns the draft, dismissed by
 * Escape, the scrim, or its own controls; focus lands inside on mount. Kept
 * deliberately small — drafts live in the caller, facts stay in the view.
 */
export function AppDialog({ title, description, onDismiss, children }: {
  title: string; description?: string; onDismiss(): void; children: ReactNode;
}) {
  const body = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const target = body.current?.querySelector<HTMLElement>("input, textarea, select, button");
    target?.focus();
    return () => previous?.focus();
  }, []);
  return <div className="dialog-overlay" role="presentation"
    onMouseDown={(event) => { if (event.target === event.currentTarget) onDismiss(); }}
    onKeyDown={(event) => { if (event.key === "Escape") { event.stopPropagation(); onDismiss(); } }}>
    <div ref={body} className="dialog-body" role="dialog" aria-modal aria-label={title}>
      <header><h2>{title}</h2>{description !== undefined && <p>{description}</p>}</header>
      {children}
    </div>
  </div>;
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
