/**
 * The address-book window — a permanently portrait rolodex, not a workspace.
 *
 * Authored Cards for this identity. Drafts (search, dialog fields) live here.
 * Facts come from `ClientView.book`. Dispatch is the only write.
 */
import { useEffect, useState } from "react";

import { actionKey, summonOwnedWindow, type Book, type Card, type ClientAction, type ClientView, type Contact } from "./client";
import { AiMark, AppDialog, Badge, DialogFooter, Empty, FacePlate, PersonTile, isAgentCard, presenceLabel } from "./kit";

type Dispatch = (action: ClientAction) => Promise<void>;

/** Search over what a card says about itself: name, note, id, handles. */
export function filterCards(cards: Card[], query: string): Card[] {
  const q = query.trim().toLowerCase();
  if (q === "") return cards;
  return cards.filter((card) =>
    card.name.toLowerCase().includes(q)
    || card.note.toLowerCase().includes(q)
    || card.card.toLowerCase().includes(q)
    || card.handles.some((handle) => handle.toLowerCase().includes(q)));
}

/**
 * The claimed card never draws as a list row: the canonical band at the top
 * of the window is its one presentation, so the list holds everyone else.
 */
export function listedCards(cards: Card[]): Card[] {
  return cards.filter((card) => !card.selfClaim);
}

/**
 * Presence parts the list, not kind: everyone reachable — or not askable —
 * sits together, ordered by how present they are, and only the measured
 * absence gets a section of its own. An unmeasured card stays up top:
 * "could not be asked" is not a lesser Offline.
 */
export function partCards(cards: Card[]): { contacts: Card[]; offline: Card[] } {
  return {
    contacts: [
      ...cards.filter((card) => card.presence === "online"),
      ...cards.filter((card) => card.presence === "away"),
      ...cards.filter((card) => card.presence === null),
    ],
    offline: cards.filter((card) => card.presence === "offline"),
  };
}

type BookDialog =
  | { kind: "edit"; card: Card }
  | { kind: "link"; card: Card }
  | { kind: "picture"; card: Card }
  | { kind: "merge"; card: Card }
  | { kind: "delete"; card: Card };

export function BookSurface({ view, dispatch, onBack, ownedWindow = false }: {
  view: ClientView; dispatch: Dispatch; onBack(): void; ownedWindow?: boolean;
}) {
  const [query, setQuery] = useState("");
  const [searching, setSearching] = useState(false);
  // The card whose profile subsurface is open, held as an id, never a row:
  // the row is re-read from the book every render, so a profile can never
  // show a card the book no longer holds.
  const [profile, setProfile] = useState<string | null>(null);
  const [dialog, setDialog] = useState<BookDialog | null>(null);
  const [incoming, setIncoming] = useState(false);

  const book = view.book;
  // The people this identity can hold a conversation with. Clicking one opens
  // a chat — the address book is the way in, never a separate inbox. Known
  // (added) contacts are the list; unknown strangers sit behind the CONTACTS
  // band's badge, revealed only when asked for.
  const messageContacts = view.correspondence?.contacts ?? [];
  const knownContacts = messageContacts.filter((contact) => contact.added);
  const unknownContacts = messageContacts.filter((contact) => !contact.added);
  // Never strand on an empty Incoming panel if the last stranger cleared.
  const showingIncoming = incoming && unknownContacts.length > 0;
  const profiled = profile === null ? null : book?.cards.find((card) => card.card === profile) ?? null;

  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "f") {
        event.preventDefault();
        setSearching(true);
      }
      // Escape peels one layer: a dialog first, then the profile, then the
      // search draft. A focused dialog consumes its own Escape before this.
      if (event.key === "Escape") {
        if (dialog !== null) { setDialog(null); return; }
        if (profile !== null) { setProfile(null); return; }
        setSearching(false);
        setQuery("");
      }
    };
    window.addEventListener("keydown", shortcut);
    return () => window.removeEventListener("keydown", shortcut);
  }, [dialog, profile]);

  const mine = book?.cards.find((card) => card.selfClaim) ?? null;
  const listed = listedCards(book?.cards ?? []);
  const shown = filterCards(listed, query);
  const { contacts, offline } = partCards(shown);
  const busy = (key: string) => view.inFlight.includes(key);

  return <section className="book-window" aria-label="Address book">
    <header className="book-top">
      <button className="back-button" onClick={onBack}>{ownedWindow ? "Close window" : "← Library"}</button>
    </header>
    {book === null && messageContacts.length === 0
      ? <div className="book-gutter"><Empty said="The book has not been read."
          next="Press F5 to ask the daemon. Nothing is created on your behalf." /></div>
      // The same CONTACTS band, standalone: the known/unknown split holds
      // even with no daemon-backed book behind it.
      : book === null
      ? <>
        <div className="contacts-strip">
          <span className="strip-title">CONTACTS</span>
          {unknownContacts.length > 0 && <IncomingButton count={unknownContacts.length}
            active={showingIncoming} onToggle={() => setIncoming(!showingIncoming)} />}
        </div>
        <div className="book-list">
          {showingIncoming
            ? unknownContacts.map((contact) => <MessageContactRow key={contact.id} contact={contact}
                view={view} dispatch={dispatch} incoming />)
            : knownContacts.length === 0
              ? <span className="muted">No conversations.</span>
              : <>
                <SectionHead label="Messages" />
                {knownContacts.map((contact) => <MessageContactRow key={contact.id} contact={contact}
                  view={view} dispatch={dispatch} />)}
              </>}
        </div>
      </>
      : profiled !== null
        ? <ProfilePage card={profiled} all={book.cards} view={view} dispatch={dispatch}
            onBack={() => setProfile(null)} openDialog={setDialog} />
        : <>
          <div className="book-lead">
            <CanonicalCard mine={mine} hostAnswered={view.host !== null}
              onOpen={mine === null ? undefined : () => setProfile(mine.card)} />
            {book.migrationPending > 0 && <p className="migration-line">
              {book.migrationPending} alias selector(s) still pending. They were not turned into Cards.
            </p>}
            {book.suggestions.length > 0 && <SuggestionBand book={book} busy={busy} dispatch={dispatch} />}
          </div>
          <div className="contacts-strip">
            {searching
              ? <>
                <input className="book-search" placeholder="Search cards" autoFocus value={query}
                  onChange={(event) => setQuery(event.target.value)} aria-label="Search cards" />
                <button className="strip-icon" aria-label="Cancel search" title="Cancel search (Esc)"
                  onClick={() => { setSearching(false); setQuery(""); }}>×</button>
              </>
              : <>
                <span className="strip-title">CONTACTS</span>
                <button className="strip-icon" aria-label="Search cards" title="Search cards (Ctrl+F)"
                  onClick={() => setSearching(true)}>⌕</button>
                {unknownContacts.length > 0 && <IncomingButton count={unknownContacts.length}
                  active={showingIncoming} onToggle={() => setIncoming(!showingIncoming)} />}
              </>}
          </div>
          <div className="book-list">
            {showingIncoming
              // The Incoming panel: strangers the band's badge counts, shown
              // only when its button is lit — never in the list.
              ? unknownContacts.map((contact) => <MessageContactRow key={contact.id} contact={contact}
                  view={view} dispatch={dispatch} incoming />)
              : shown.length === 0 && knownContacts.length === 0
              ? <div className="book-gutter"><Empty
                  said={listed.length === 0 ? "No cards." : "No cards match that search."}
                  next={listed.length === 0
                    ? "The book is this identity's, even with no Space open."
                    : "Clear the search to see every Card."} /></div>
              : <>
                {knownContacts.length > 0 && <>
                  <SectionHead label="Messages" />
                  {knownContacts.map((contact) => <MessageContactRow key={contact.id} contact={contact}
                    view={view} dispatch={dispatch} />)}
                  <hr className="book-rule" />
                </>}
                {contacts.length > 0 && <>
                  <SectionHead label="Contacts" count={contacts.length} />
                  {contacts.map((card) => <PersonRow key={card.card} card={card} onOpen={() => setProfile(card.card)} />)}
                </>}
                {contacts.length > 0 && offline.length > 0 && <hr className="book-rule" />}
                {offline.length > 0 && <>
                  <SectionHead label="Offline" />
                  {offline.map((card) => <PersonRow key={card.card} card={card} onOpen={() => setProfile(card.card)} />)}
                </>}
              </>}
          </div>
        </>}
    {dialog !== null && <BookDialogs dialog={dialog} all={book?.cards ?? []} dispatch={dispatch} onDismiss={() => setDialog(null)} />}
  </section>;
}

/**
 * The canonical card: how an identity is presented anywhere a person appears
 * — a picture, a name, a status. The book leads with your own. The status
 * line is derived, never asserted: measured presence on the card's own
 * handles when a Space answered, else the local fact — this identity's daemon
 * answering this very read. When even that is absent, the line is absent too.
 */
function CanonicalCard({ mine, hostAnswered, onOpen }: { mine: Card | null; hostAnswered: boolean; onOpen?: () => void }) {
  const name = mine === null ? "No My Card." : mine.name;
  const status = mine === null
    ? "Claim one — nothing is implied from a name or a handle."
    : presenceLabel(mine.presence) ?? (hostAnswered ? "Online" : null);
  return <div className="canonical-card" onDoubleClick={onOpen} role={onOpen === undefined ? undefined : "button"}
    aria-label={mine === null ? "No My Card" : `Open the profile of ${mine.name}`}>
    <FacePlate picture={mine?.picture ?? null} name={mine?.name ?? ""} size={56} />
    <span className="person-copy">
      <span className="person-name"><strong className="canonical-name">{name}</strong></span>
      {status !== null && <small>{status}</small>}
    </span>
  </div>;
}

function SectionHead({ label, count }: { label: string; count?: number }) {
  return <div className="book-section-head"><span>{label}</span>{count !== undefined && <span className="dim">({count})</span>}</div>;
}

/**
 * The door to the unknown, in the CONTACTS band beside search: a button with
 * a count badge, the way Steam badges incoming requests.
 */
function IncomingButton({ count, active, onToggle }: { count: number; active: boolean; onToggle(): void }) {
  return <button className="strip-icon incoming-button" data-active={active || undefined}
    aria-label={`Incoming from unknown senders (${count})`} title={active ? "Back to contacts" : "Incoming"}
    onClick={onToggle}>
    ⊕<span className="incoming-count">{count}</span>
  </button>;
}

/**
 * A person one can message, drawn with the same canonical tile as every other
 * row: the AI mark for an agent, a note for whose agent it is, and an unread
 * badge. A single tap opens (or focuses) their chat — the address book is the
 * way in, so the contact row *is* the entry.
 */
function MessageContactRow({ contact, view, dispatch, incoming = false }: {
  contact: Contact; view: ClientView; dispatch: Dispatch; incoming?: boolean;
}) {
  return <div className="person-row message-row" role="button" aria-label={`Open chat with ${contact.name}`}
    onClick={() => {
      void dispatch({ type: "openConversation", person: contact.id });
      void summonOwnedWindow("chat");
    }}>
    <PersonTile name={contact.name} picture={null} presence={null} agent={contact.isAgent}
      note={contact.parentName === null ? undefined : `${contact.parentName}'s agent`}
      trailing={incoming
        ? <RequestActions person={contact.id} name={contact.name} view={view} dispatch={dispatch} />
        : contact.unread > 0 ? <Badge label={`${contact.unread}`} solid /> : undefined} />
  </div>;
}

/**
 * Accept moves an unknown sender into the address book; dismiss blocks them
 * at the carrier. The two acts on a request, ✓ and ✗, flush at the row's end.
 */
function RequestActions({ person, name, view, dispatch }: {
  person: string; name: string; view: ClientView; dispatch: Dispatch;
}) {
  const stop = (event: React.MouseEvent) => event.stopPropagation();
  return <span className="button-row" onClick={stop}>
    <button className="strip-icon" aria-label={`Accept ${name}`} title="Accept"
      disabled={view.inFlight.includes(actionKey.acceptContact(person))}
      onClick={() => void dispatch({ type: "acceptContact", person })}>✓</button>
    <button className="strip-icon danger-text" aria-label={`Dismiss ${name}`} title="Dismiss"
      disabled={view.inFlight.includes(actionKey.blockSender(person))}
      onClick={() => void dispatch({ type: "blockSender", person })}>✗</button>
  </span>;
}

function PersonRow({ card, onOpen }: { card: Card; onOpen(): void }) {
  return <button className="person-row" onClick={onOpen} aria-label={`Open the profile of ${card.name}`}>
    <PersonTile name={card.name} picture={card.picture} presence={card.presence} agent={isAgentCard(card)}
      note={card.note} />
  </button>;
}

/**
 * The profile page — the subsurface a card's whole truth lives on, rendered
 * in the window in place of the list. Back (or Escape) returns.
 */
function ProfilePage({ card, all, view, dispatch, onBack, openDialog }: {
  card: Card; all: Card[]; view: ClientView; dispatch: Dispatch; onBack(): void; openDialog(dialog: BookDialog): void;
}) {
  const busy = (key: string) => view.inFlight.includes(key);
  return <div className="book-gutter profile-page">
    <div><button className="back-button" onClick={onBack} title="Back (Esc)">← Back</button></div>
    <div className="profile-head">
      <FacePlate picture={card.picture} name={card.name} size={56} />
      <span className="person-copy">
        <span className="person-name">
          <strong className="canonical-name">{card.name}</strong>
          {isAgentCard(card) && <AiMark />}
          {card.selfClaim && <Badge label="My Card" solid />}
        </span>
        {card.note !== "" && <small>{card.note}</small>}
      </span>
    </div>
    <div className="profile-scroll">
      <HandleSection label="ADDRESSES" handles={card.addresses} card={card.card} view={view} dispatch={dispatch} />
      <HandleSection label="DEVICES" handles={card.devices} card={card.card} view={view} dispatch={dispatch} />
      <HandleSection label="AGENTS" handles={card.agents} card={card.card} view={view} dispatch={dispatch} />
      <div className="profile-actions">
        <button className="quiet-button" disabled={busy(actionKey.bookPut(card.card))}
          onClick={() => openDialog({ kind: "edit", card })}>Edit</button>
        <button className="quiet-button" disabled={busy(actionKey.bookSetPicture(card.card))}
          onClick={() => openDialog({ kind: "picture", card })}>Set picture</button>
        {card.picture !== null && <button className="quiet-button" disabled={busy(actionKey.bookSetPicture(card.card))}
          onClick={() => void dispatch({ type: "bookSetPicture", card: card.card, path: null })}>Clear picture</button>}
        {!card.selfClaim && <button className="quiet-button" disabled={busy(actionKey.bookClaimSelf(card.card))}
          onClick={() => void dispatch({ type: "bookClaimSelf", card: card.card })}>Claim as My Card</button>}
        <button className="quiet-button" disabled={busy(actionKey.bookLink(card.card))}
          onClick={() => openDialog({ kind: "link", card })}>Add handle</button>
        {all.length > 1 && <button className="quiet-button" onClick={() => openDialog({ kind: "merge", card })}>Merge</button>}
        <button className="danger-button" disabled={busy(actionKey.bookDelete(card.card))}
          onClick={() => openDialog({ kind: "delete", card })}>Delete</button>
      </div>
    </div>
  </div>;
}

/**
 * One phone-book section: a label and its rows, each unlinkable. Absent kinds
 * draw nothing — a card with no devices has no DEVICES heading.
 */
function HandleSection({ label, handles, card, view, dispatch }: {
  label: string; handles: string[]; card: string; view: ClientView; dispatch: Dispatch;
}) {
  if (handles.length === 0) return null;
  const busy = view.inFlight.includes(actionKey.bookUnlink(card));
  return <section className="handle-section">
    <span className="fact-label">{label}</span>
    {handles.map((handle) => <div className="handle-row" key={handle}>
      <code>{handle}</code>
      <button className="text-button" disabled={busy} aria-label={`Unlink ${handle}`}
        onClick={() => void dispatch({ type: "bookUnlink", card, handle })}>Unlink</button>
    </div>)}
  </section>;
}

/**
 * Staged suggestions from card-exchange files. Review is the only way into
 * the book: each row is accepted or dismissed, never silently applied.
 */
function SuggestionBand({ book, busy, dispatch }: { book: Book; busy(key: string): boolean; dispatch: Dispatch }) {
  return <section className="suggestion-band">
    <strong>{book.suggestions.length} suggested from files</strong>
    <small>Nothing below is in the book until you accept it.</small>
    {book.suggestions.map((suggestion) => <div className="suggestion-row" key={suggestion.suggestion}>
      <span className="person-copy">
        <span>{suggestion.name}</span>
        {suggestion.note !== "" && <small>{suggestion.note}</small>}
        {suggestion.handles.map((handle) => <code key={handle}>{handle}</code>)}
      </span>
      <span className="button-row">
        <button className="primary-button" disabled={busy(actionKey.bookAccept(suggestion.suggestion))}
          onClick={() => void dispatch({ type: "bookAccept", suggestion: suggestion.suggestion })}>Accept</button>
        <button className="quiet-button" disabled={busy(actionKey.bookDismiss(suggestion.suggestion))}
          onClick={() => void dispatch({ type: "bookDismiss", suggestion: suggestion.suggestion })}>Dismiss</button>
      </span>
    </div>)}
  </section>;
}

function BookDialogs({ dialog, all, dispatch, onDismiss }: {
  dialog: BookDialog; all: Card[]; dispatch: Dispatch; onDismiss(): void;
}) {
  switch (dialog.kind) {
    case "edit": return <EditDialog card={dialog.card} dispatch={dispatch} onDismiss={onDismiss} />;
    case "link": return <LinkDialog card={dialog.card} dispatch={dispatch} onDismiss={onDismiss} />;
    case "picture": return <PictureDialog card={dialog.card} dispatch={dispatch} onDismiss={onDismiss} />;
    case "merge": return <MergeDialog card={dialog.card} all={all} dispatch={dispatch} onDismiss={onDismiss} />;
    case "delete": return <DeleteDialog card={dialog.card} dispatch={dispatch} onDismiss={onDismiss} />;
  }
}

function EditDialog({ card, dispatch, onDismiss }: { card: Card; dispatch: Dispatch; onDismiss(): void }) {
  const [name, setName] = useState(card.name);
  const [note, setNote] = useState(card.note);
  const save = () => {
    const trimmed = name.trim();
    if (trimmed === "") return;
    void dispatch({ type: "bookPut", card: card.card, name: trimmed, note: note.trim() || null });
    onDismiss();
  };
  return <AppDialog title="Edit card"
    description="A name is an authored label. It never selects an authority target." onDismiss={onDismiss}>
    <label>Name<input value={name} onChange={(event) => setName(event.target.value)} /></label>
    <label>Note<input value={note} onChange={(event) => setNote(event.target.value)} /></label>
    <DialogFooter>
      <button className="quiet-button" onClick={onDismiss}>Cancel</button>
      <button className="primary-button" disabled={name.trim() === ""} onClick={save}>Save</button>
    </DialogFooter>
  </AppDialog>;
}

function LinkDialog({ card, dispatch, onDismiss }: { card: Card; dispatch: Dispatch; onDismiss(): void }) {
  const [handle, setHandle] = useState("");
  return <AppDialog title={`Add a handle to ${card.name}`}
    description="Wire spelling: a device id, actor:space:actor, or agent:hash:name." onDismiss={onDismiss}>
    <label>Handle<input className="mono" value={handle} onChange={(event) => setHandle(event.target.value)} /></label>
    <DialogFooter>
      <button className="quiet-button" onClick={onDismiss}>Cancel</button>
      <button className="primary-button" disabled={handle.trim() === ""} onClick={() => {
        void dispatch({ type: "bookLink", card: card.card, handle: handle.trim() });
        onDismiss();
      }}>Link</button>
    </DialogFooter>
  </AppDialog>;
}

function PictureDialog({ card, dispatch, onDismiss }: { card: Card; dispatch: Dispatch; onDismiss(): void }) {
  const [path, setPath] = useState("");
  return <AppDialog title={`Set the picture on ${card.name}`}
    description="A PNG, JPEG, or WebP on this machine. The book stores the picture itself — keep it face-sized, not a photo."
    onDismiss={onDismiss}>
    <label>Path<input className="mono" value={path} onChange={(event) => setPath(event.target.value)} /></label>
    <DialogFooter>
      <button className="quiet-button" onClick={onDismiss}>Cancel</button>
      <button className="primary-button" disabled={path.trim() === ""} onClick={() => {
        void dispatch({ type: "bookSetPicture", card: card.card, path: path.trim() });
        onDismiss();
      }}>Set picture</button>
    </DialogFooter>
  </AppDialog>;
}

function MergeDialog({ card, all, dispatch, onDismiss }: { card: Card; all: Card[]; dispatch: Dispatch; onDismiss(): void }) {
  const others = all.filter((other) => other.card !== card.card);
  const [into, setInto] = useState(others[0]?.card ?? "");
  const [typed, setTyped] = useState("");
  const confirmed = typed.trim() === card.name;
  if (others.length === 0) return null;
  return <AppDialog title={`Merge ${card.name}?`}
    description={`This Card is absorbed into another. Type ${card.name} to confirm.`} onDismiss={onDismiss}>
    <label>Merge into<select value={into} onChange={(event) => setInto(event.target.value)}>
      {others.map((other) => <option key={other.card} value={other.card}>{other.name}</option>)}
    </select></label>
    <label>Confirm<input placeholder={card.name} value={typed} onChange={(event) => setTyped(event.target.value)}
      aria-label="Type this card's name to confirm" /></label>
    <DialogFooter>
      <button className="quiet-button" onClick={onDismiss}>Cancel</button>
      <button className="danger-button" disabled={!confirmed}
        title={confirmed ? undefined : "Type this card's name to confirm."} onClick={() => {
          void dispatch({ type: "bookMerge", from: card.card, into });
          onDismiss();
        }}>Merge</button>
    </DialogFooter>
  </AppDialog>;
}

function DeleteDialog({ card, dispatch, onDismiss }: { card: Card; dispatch: Dispatch; onDismiss(): void }) {
  const [typed, setTyped] = useState("");
  const confirmed = typed.trim() === card.name;
  return <AppDialog title={`Delete ${card.name}?`}
    description={`This cannot be undone. Type ${card.name} to confirm.`} onDismiss={onDismiss}>
    <label>Confirm<input placeholder={card.name} value={typed} onChange={(event) => setTyped(event.target.value)}
      aria-label="Type this card's name to confirm" /></label>
    <DialogFooter>
      <button className="quiet-button" onClick={onDismiss}>Cancel</button>
      <button className="danger-button" disabled={!confirmed}
        title={confirmed ? undefined : "Type this card's name to confirm."} onClick={() => {
          void dispatch({ type: "bookDelete", card: card.card });
          onDismiss();
        }}>Delete</button>
    </DialogFooter>
  </AppDialog>;
}
