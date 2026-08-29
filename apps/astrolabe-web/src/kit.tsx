/**
 * The canonical presentation pieces every Astrolabe surface shares — the face
 * on a card, the person row, badges, fact rows, and the app dialog. These are
 * the web spellings of the Flutter client's `face.dart` / `person.dart`
 * anatomy: a tile draws facts and nothing else; a surface composes gestures
 * and controls around it.
 *
 * Drawn with Fluent. A surface that needs a button, a field or a card takes
 * it from `@fluentui/react-components` directly; what lives here is the
 * handful of compositions Astrolabe repeats.
 */
import {
  Badge as FluentBadge,
  Caption1,
  Caption1Strong,
  Dialog,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  MessageBar,
  MessageBarBody,
  Text,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import type { ReactNode } from "react";

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
  // A badge is one word or two and never breaks: a card's action slot is
  // narrow, and a label folded onto two lines reads as two badges.
  return <FluentBadge appearance={solid ? "filled" : "outline"} color={solid ? "brand" : "informative"} size="medium" shape="rounded"
    style={{ whiteSpace: "nowrap", flexShrink: 0 }}>
    {label}
  </FluentBadge>;
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

const useKitStyles = makeStyles({
  fact: {
    display: "grid",
    gridTemplateColumns: "minmax(120px, 160px) minmax(0, 1fr)",
    columnGap: tokens.spacingHorizontalM,
    alignItems: "baseline",
    paddingTop: tokens.spacingVerticalXXS,
    paddingBottom: tokens.spacingVerticalXXS,
  },
  factLabel: { color: tokens.colorNeutralForeground3, textTransform: "uppercase", letterSpacing: "0.06em" },
  factValue: { fontFamily: tokens.fontFamilyMonospace, fontSize: tokens.fontSizeBase200, overflowWrap: "anywhere", userSelect: "all" },
  section: {
    display: "flex",
    justifyContent: "space-between",
    alignItems: "baseline",
    marginTop: tokens.spacingVerticalL,
    marginBottom: tokens.spacingVerticalS,
    color: tokens.colorNeutralForeground3,
    textTransform: "uppercase",
    letterSpacing: "0.08em",
  },
  empty: {
    display: "grid",
    gap: tokens.spacingVerticalXS,
    alignContent: "center",
    minHeight: "96px",
    padding: tokens.spacingVerticalL,
    textAlign: "center",
    border: `1px dashed ${tokens.colorNeutralStroke2}`,
    borderRadius: tokens.borderRadiusLarge,
    color: tokens.colorNeutralForeground3,
  },
  dialogStack: { display: "grid", gap: tokens.spacingVerticalM, marginTop: tokens.spacingVerticalS },
  dialogFooter: {
    display: "flex",
    justifyContent: "flex-end",
    gap: tokens.spacingHorizontalS,
    marginTop: tokens.spacingVerticalM,
  },
});

/** A labelled fact row whose value is selectable, never truncated by hand. */
export function Fact({ label, value }: { label: string; value: string }) {
  const styles = useKitStyles();
  return <div className={styles.fact}>
    <Caption1 className={styles.factLabel}>{label}</Caption1>
    <Text className={styles.factValue}>{value}</Text>
  </div>;
}

/** A section marking: an emphatic label with the count dim beside it. */
export function SectionTitle({ label, count }: { label: string; count?: number }) {
  const styles = useKitStyles();
  return <div className={styles.section}>
    <Caption1Strong>{label}</Caption1Strong>
    {count !== undefined && <Caption1>{count}</Caption1>}
  </div>;
}

export function Empty({ said, next }: { said: string; next?: string }) {
  const styles = useKitStyles();
  return <div className={styles.empty}>
    <Text weight="semibold">{said}</Text>
    {next !== undefined && <Caption1>{next}</Caption1>}
  </div>;
}

export function Notice({ tone, children }: { tone: "warn" | "danger" | "good"; children: ReactNode }) {
  const intent = tone === "danger" ? "error" : tone === "warn" ? "warning" : "success";
  return <MessageBar intent={intent} layout="multiline">
    <MessageBarBody>{children}</MessageBarBody>
  </MessageBar>;
}

/**
 * The app dialog. Rendered by the surface that owns the draft, dismissed by
 * Escape, the scrim, or its own controls; focus lands inside on mount. Kept
 * deliberately small — drafts live in the caller, facts stay in the view.
 *
 * The content carries the `dialog-fields` class so a surface not yet moved to
 * Fluent fields draws its plain labels and inputs the way it did — the field
 * rules only, never the old panel.
 */
export function AppDialog({ title, description, onDismiss, children }: {
  title: string; description?: string; onDismiss(): void; children: ReactNode;
}) {
  const styles = useKitStyles();
  return <Dialog open modalType="modal" onOpenChange={(_, data) => { if (!data.open) onDismiss(); }}>
    <DialogSurface aria-label={title}>
      <DialogBody>
        <DialogTitle>{title}</DialogTitle>
        <DialogContent>
          {description !== undefined && <Text block>{description}</Text>}
          <div className={`dialog-fields ${styles.dialogStack}`}>{children}</div>
        </DialogContent>
      </DialogBody>
    </DialogSurface>
  </Dialog>;
}

export function DialogFooter({ children }: { children: ReactNode }) {
  const styles = useKitStyles();
  return <footer className={styles.dialogFooter}>{children}</footer>;
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
