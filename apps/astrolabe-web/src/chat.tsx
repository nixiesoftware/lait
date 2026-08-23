/**
 * The chat window.
 *
 * Correspondence is a conversation, never an inbox. A person is reached from
 * the address book; a click opens a chat here. The chrome is its tabs —
 * browser-style, each with the correspondent's face as its favicon. It draws
 * only what the shared model holds: which tabs are open, which is focused,
 * and every message are `ClientView.correspondence` — the same view the
 * address book reads. The one draft it owns is the line being typed.
 */
import { useRef, useState } from "react";

import {
  actionKey,
  type ChatMessage,
  type ClientAction,
  type ClientView,
  type Conversation,
  type CorrespondenceFacts,
} from "./client";
import { Badge, FacePlate } from "./kit";

type Dispatch = (action: ClientAction) => Promise<void>;

/**
 * A run of same-sender messages is broken when the sender flips, the day
 * turns, or this much quiet passes — the gap that earns a divider.
 */
export const longGapSeconds = 60 * 60;

export type TranscriptItem =
  | { kind: "day"; label: string }
  | { kind: "gap" }
  | { kind: "message"; message: ChatMessage; groupStarts: boolean; groupEnds: boolean };

/**
 * The transcript's shape: messages grouped by sender and parted by day and
 * long quiet. `today` is passed in (unix seconds) so the shape is a pure
 * function of its inputs.
 */
export function transcriptItems(messages: ChatMessage[], now: Date = new Date()): TranscriptItem[] {
  const items: TranscriptItem[] = [];
  let prev: Date | null = null;
  for (let index = 0; index < messages.length; index += 1) {
    const message = messages[index];
    const at = atOf(message);
    const next = index + 1 < messages.length ? messages[index + 1] : null;

    const newDay = prev === null || !sameDay(prev, at);
    const longGap = prev !== null && !newDay && at.getTime() - prev.getTime() > longGapSeconds * 1000;
    if (newDay) items.push({ kind: "day", label: dayLabel(at, now) });
    else if (longGap) items.push({ kind: "gap" });

    const groupStarts = newDay || longGap || index === 0 || messages[index - 1].mine !== message.mine;
    const nextAt = next === null ? null : atOf(next);
    const groupEnds = next === null
      || next.mine !== message.mine
      || !sameDay(at, nextAt!)
      || nextAt!.getTime() - at.getTime() > longGapSeconds * 1000;

    items.push({ kind: "message", message, groupStarts, groupEnds });
    prev = at;
  }
  return items;
}

function atOf(message: ChatMessage): Date {
  return new Date(message.sentAt * 1000);
}

function sameDay(a: Date, b: Date): boolean {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}

const weekdays = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** A centered date pill: Today, Yesterday, or a written date. */
export function dayLabel(at: Date, now: Date = new Date()): string {
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const that = new Date(at.getFullYear(), at.getMonth(), at.getDate());
  const days = Math.round((today.getTime() - that.getTime()) / 86_400_000);
  if (days === 0) return "Today";
  if (days === 1) return "Yesterday";
  return `${weekdays[at.getDay()]}, ${months[at.getMonth()]} ${at.getDate()}`;
}

export function timeLabel(at: Date): string {
  const hour = at.getHours() % 12 === 0 ? 12 : at.getHours() % 12;
  const minute = at.getMinutes().toString().padStart(2, "0");
  return `${hour}:${minute} ${at.getHours() < 12 ? "AM" : "PM"}`;
}

export function ChatSurface({ view, dispatch, onBack, ownedWindow = false }: {
  view: ClientView; dispatch: Dispatch; onBack(): void; ownedWindow?: boolean;
}) {
  const facts = view.correspondence;
  const tabs = facts?.openTabs ?? [];
  const active = facts?.activeTab ?? tabs[0] ?? null;
  return <section className="chat-window" aria-label="Chat">
    <header className="chat-tabs-band">
      <button className="back-button" onClick={onBack}>{ownedWindow ? "Close window" : "← Library"}</button>
      {tabs.length === 0
        ? <span className="chat-band-title">Chat</span>
        : <div className="chat-tabs">
          {tabs.map((id) => <ChatTab key={id} facts={facts!} id={id} active={id === active}
            onFocus={() => void dispatch({ type: "focusConversation", person: id })}
            onClose={() => void dispatch({ type: "closeConversation", person: id })} />)}
        </div>}
    </header>
    <ChatBody view={view} facts={facts} active={active} dispatch={dispatch} />
  </section>;
}

function ChatTab({ facts, id, active, onFocus, onClose }: {
  facts: CorrespondenceFacts; id: string; active: boolean; onFocus(): void; onClose(): void;
}) {
  const name = nameOf(facts, id);
  return <div className={active ? "chat-tab active" : "chat-tab"} role="tab" aria-selected={active}
    onClick={onFocus}>
    <FacePlate picture={null} name={name} size={18} />
    <span className="chat-tab-name">{name}</span>
    {isAgent(facts, id) && <Badge label="AI" />}
    <button className="chat-tab-close" aria-label={`Close chat with ${name}`}
      onClick={(event) => { event.stopPropagation(); onClose(); }}>×</button>
  </div>;
}

function ChatBody({ view, facts, active, dispatch }: {
  view: ClientView; facts: CorrespondenceFacts | null; active: string | null; dispatch: Dispatch;
}) {
  // One draft per peer, so switching tabs never loses a half-written line.
  const drafts = useRef(new Map<string, string>());
  const [, bump] = useState(0);
  // The invitation being carried, per peer, for the same reason.
  const invites = useRef(new Map<string, string>());
  const [carrying, setCarrying] = useState<string | null>(null);
  const [showReach, setShowReach] = useState(false);

  if (facts === null || active === null) {
    // Nothing open is the moment reach is worth offering: a person with no
    // correspondents cannot open a conversation, so a bare "open one" is a
    // dead end for exactly the person who needs this most.
    return <div className="chat-empty">
      <p>Open a conversation from the address book.</p>
      <ReachPanel view={view} dispatch={dispatch} />
    </div>;
  }
  const conversation = facts.conversations.find((row) => row.peerId === active);
  if (conversation === undefined) return <div className="chat-empty" />;

  const draft = drafts.current.get(active) ?? "";
  const sending = view.inFlight.includes(actionKey.sendMessage(active));
  const send = () => {
    const body = draft.trim();
    if (body === "" || sending) return;
    void dispatch({ type: "sendMessage", to: active, body });
    drafts.current.set(active, "");
    bump((n) => n + 1);
  };

  return <div className="chat-body">
    <div className="chat-header">
      <FacePlate picture={null} name={conversation.peerName} size={28} />
      <strong>{conversation.peerName}</strong>
      {isAgent(facts, active) && <Badge label="AI" />}
      <span className="chat-header-spring" />
      <button className="quiet-button" title="Reach someone new"
        aria-pressed={showReach}
        onClick={() => setShowReach(!showReach)}>✚</button>
      <button className="quiet-button" title="Check for messages"
        disabled={view.inFlight.includes(actionKey.collectMail)}
        onClick={() => void dispatch({ type: "collectMail" })}>↻</button>
      <button className="danger-button" disabled={view.inFlight.includes(actionKey.blockSender(active))}
        onClick={() => void dispatch({ type: "blockSender", person: active })}>Block</button>
    </div>
    {showReach && <ReachPanel view={view} dispatch={dispatch} />}
    <Transcript conversation={conversation} view={view} dispatch={dispatch} />
    {carrying === active && <InvitationComposer
      to={active}
      link={invites.current.get(active) ?? ""}
      sending={view.inFlight.includes(actionKey.sendInvitation(active))}
      onChange={(link) => { invites.current.set(active, link); bump((n) => n + 1); }}
      onSend={(link) => {
        void dispatch({ type: "sendInvitation", to: active, link });
        invites.current.delete(active);
        setCarrying(null);
      }}
      onCancel={() => setCarrying(null)}
    />}
    <div className="chat-composer">
      <span className="tip" title="Attaching files is not available yet">
        <button className="rail-button" aria-label="Attach a file (not available yet)" disabled>📎</button></span>
      <button
        className="rail-button"
        aria-label="Carry an invitation"
        aria-pressed={carrying === active}
        onClick={() => setCarrying(carrying === active ? null : active)}
      >✉</button>
      <span className="tip" title="Emoji are not available yet">
        <button className="rail-button" aria-label="Emoji (not available yet)" disabled>🙂</button></span>
      <textarea className="chat-input" placeholder="Message" rows={1} value={draft}
        onChange={(event) => { drafts.current.set(active, event.target.value); bump((n) => n + 1); }}
        onKeyDown={(event) => {
          // Enter sends; Shift+Enter is a newline.
          if (event.key === "Enter" && !event.shiftKey) {
            event.preventDefault();
            send();
          }
        }} />
      <button className="chat-send" aria-label="Send" disabled={draft.trim() === "" || sending} onClick={send}>↑</button>
    </div>
  </div>;
}

/** The messages, grouped by sender and parted by day and long quiet. */
function Transcript({ conversation, view, dispatch }: {
  conversation: Conversation;
  view: ClientView;
  dispatch: Dispatch;
}) {
  if (conversation.messages.length === 0) {
    return <div className="chat-empty">No messages yet.</div>;
  }
  const items = transcriptItems(conversation.messages);
  return <div className="chat-transcript">
    {items.map((item, index) => {
      if (item.kind === "day") return <div className="chat-day" key={index}><span>{item.label}</span></div>;
      if (item.kind === "gap") return <hr className="chat-gap" key={index} />;
      return <MessageRow key={index} message={item.message} peerName={conversation.peerName}
        groupStarts={item.groupStarts} groupEnds={item.groupEnds} view={view} dispatch={dispatch} />;
    })}
  </div>;
}

/**
 * One message: on the side of whoever sent it, its component chosen by kind,
 * with the sender's name+time above the first of a group and their face below
 * the last.
 */
function MessageRow({ message, peerName, groupStarts, groupEnds, view, dispatch }: {
  message: ChatMessage; peerName: string; groupStarts: boolean; groupEnds: boolean;
  view: ClientView; dispatch: Dispatch;
}) {
  const at = atOf(message);
  if (message.mine) {
    return <div className="chat-row mine" data-group-ends={groupEnds || undefined}>
      <MessageComponent message={message} view={view} dispatch={dispatch} />
      {groupEnds && <small className="chat-time">{timeLabel(at)}</small>}
    </div>;
  }
  return <div className="chat-row theirs" data-group-ends={groupEnds || undefined}>
    <span className="chat-gutter">{groupEnds && <FacePlate picture={null} name={peerName} size={28} />}</span>
    <div className="chat-column">
      {groupStarts && <span className="chat-byline"><strong>{peerName}</strong><small>{timeLabel(at)}</small></span>}
      <MessageComponent message={message} view={view} dispatch={dispatch} />
    </div>
  </div>;
}

/**
 * The seam where a message kind chooses its component. A new kind adds a case
 * here and its own widget; nothing else in the chat changes.
 */
function MessageComponent({ message, view, dispatch }: {
  message: ChatMessage;
  view: ClientView;
  dispatch: Dispatch;
}) {
  switch (message.kind) {
    case "invitation": return <InvitationCard message={message} view={view} dispatch={dispatch} />;
    default: return <TextBubble message={message} />;
  }
}

function TextBubble({ message }: { message: ChatMessage }) {
  return <div className={message.mine ? "chat-bubble mine" : "chat-bubble"}>
    <span>{message.body ?? ""}</span>
    {!message.mine && !message.provenanceAgrees
      && <small className="provenance-note">delivered by a different device</small>}
  </div>;
}

/**
 * An invitation to a Space — a widget acted on, not read. The chatbot model:
 * a message that is a card with its own affordances.
 *
 * Opening it is the same act as following an invite link: the coordinates
 * verify against their own Space, so delivery was never admission. One this
 * identity *sent* carries no deposit id, and there is nothing there to accept —
 * which is why the control is absent rather than present and dead.
 */
function InvitationCard({ message, view, dispatch }: {
  message: ChatMessage;
  view: ClientView;
  dispatch: Dispatch;
}) {
  const id = message.id;
  const opening = id !== null && view.inFlight.includes(actionKey.openInvitation(id));
  return <div className={message.mine ? "invitation-card mine" : "invitation-card"}>
    <strong>✉ Invitation to a Space</strong>
    <p>Signed by the sender. Opening it enters the Space it names.</p>
    {!message.mine && id !== null && <span className="button-row">
      <button
        className="primary-button"
        disabled={opening}
        onClick={() => dispatch({ type: "openInvitation", message: id })}
      >{opening ? "Opening…" : "Open"}</button>
    </span>}
  </div>;
}

/**
 * Carry an invitation somebody already holds to a correspondent.
 *
 * Only carrying. Minting one is the Space's authority and stays there, which is
 * why this takes a link rather than offering a list of Spaces to invite to —
 * offering that list would imply this client can mint, and it cannot.
 *
 * The link is not parsed here. It is opaque the whole way: the recipient's
 * `OpenInvitation` hands it to `host_space_enter`, which is the only thing that
 * judges an invitation. Validating it here would be a second opinion that can
 * disagree with the one that matters.
 */
function InvitationComposer({ to, link, sending, onChange, onSend, onCancel }: {
  to: string;
  link: string;
  sending: boolean;
  onChange(link: string): void;
  onSend(link: string): void;
  onCancel(): void;
}) {
  const ready = link.trim() !== "" && !sending;
  return <div className="invitation-composer" aria-label={`Carry an invitation to ${to}`}>
    <input
      className="invitation-link"
      placeholder="Paste an invite link"
      value={link}
      onChange={(event) => onChange(event.target.value)}
      onKeyDown={(event) => {
        if (event.key === "Enter" && ready) onSend(link.trim());
        if (event.key === "Escape") onCancel();
      }}
    />
    <button className="primary-button" disabled={!ready} onClick={() => onSend(link.trim())}>
      {sending ? "Sending…" : "Send invitation"}
    </button>
    <button className="quiet-button" onClick={onCancel}>Cancel</button>
  </div>;
}

/**
 * Swap once, over any channel already shared. Yours names your devices; theirs
 * names theirs.
 *
 * Sharing acts on nobody — showing a friend code is not befriending anyone — so
 * it is `myReach` being absent that makes this a control rather than a render.
 * Adding is the half that creates a relationship, which is why it is named for
 * the person and not for the artifact.
 */
export function ReachPanel({ view, dispatch }: {
  view: ClientView;
  dispatch: Dispatch;
}) {
  const [draft, setDraft] = useState("");
  const card = view.correspondence?.myReach ?? null;
  const sharing = view.inFlight.includes(actionKey.shareReach);
  const adding = view.inFlight.includes(actionKey.addCorrespondent);

  const add = () => {
    const pasted = draft.trim();
    if (pasted === "") return;
    dispatch({ type: "addCorrespondent", announcement: pasted });
    setDraft("");
  };

  return <section className="reach-panel">
    <h3>Reach someone new</h3>
    <p>
      Swap these once, over any channel you already share. Yours names your
      devices; theirs names theirs.
    </p>
    {card === null
      ? <button
          className="quiet-button"
          disabled={sharing}
          onClick={() => dispatch({ type: "shareReach" })}
        >{sharing ? "Publishing…" : "Show how to reach me"}</button>
      : <textarea className="reach-card" readOnly rows={4} value={card} />}
    <textarea
      className="reach-paste"
      rows={3}
      placeholder="Paste theirs"
      value={draft}
      onChange={(event) => setDraft(event.target.value)}
    />
    <button className="primary-button" disabled={adding} onClick={add}>
      {adding ? "Adding…" : "Add them"}
    </button>
  </section>;
}

function nameOf(facts: CorrespondenceFacts, id: string): string {
  return facts.conversations.find((row) => row.peerId === id)?.peerName ?? id;
}

function isAgent(facts: CorrespondenceFacts, id: string): boolean {
  return facts.contacts.find((contact) => contact.id === id)?.isAgent ?? false;
}
