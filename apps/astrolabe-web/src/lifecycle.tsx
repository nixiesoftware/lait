import { useState, type ReactNode } from "react";

import {
  actionKey,
  type Book,
  type Card,
  type ClientAction,
  type ClientView,
  type Display,
  type McpBinding,
} from "./client";

export type SecondarySurface = "book" | "displays" | "record";

type SurfaceProps = {
  view: ClientView;
  dispatch(action: ClientAction): Promise<void>;
  onBack(): void;
};

export function BookSurface({ view, dispatch, onBack }: SurfaceProps) {
  const [editing, setEditing] = useState<string | "new" | null>(null);
  const book = view.book;
  const card = editing === null || editing === "new" ? undefined : book?.cards.find((item) => item.card === editing);

  if (editing !== null) return <BookEditor key={editing} card={card} all={book?.cards ?? []} view={view} dispatch={dispatch} onBack={() => setEditing(null)} />;

  return <SecondaryFrame title="Address book" detail="Your authored Cards, contacts, and staged exchanges." onBack={onBack}
    actions={<><button className="quiet-button" onClick={() => void dispatch({ type: "refresh" })}>Refresh</button><button className="primary-button" onClick={() => setEditing("new")}>New card</button></>}>
    {book === null ? <Unread what="The address book has not been read." /> : <BookList book={book} onEdit={setEditing} dispatch={dispatch} />}
  </SecondaryFrame>;
}

function BookList({ book, onEdit, dispatch }: { book: Book; onEdit(card: string): void; dispatch(action: ClientAction): Promise<void> }) {
  const [path, setPath] = useState("");
  const [exportPath, setExportPath] = useState("");
  const mine = book.cards.find((card) => card.selfClaim);
  return <div className="secondary-scroll book-surface">
    {mine !== undefined && <section className="mine-card"><span className="eyebrow">MY CARD</span><strong>{mine.name}</strong><span>{mine.note || "No note"}</span><button className="text-button" onClick={() => onEdit(mine.card)}>Edit my card</button></section>}
    {!book.migrationComplete && <Notice tone="warn">Migrating {book.migrationPending} legacy cards ({book.migrationImported} imported).</Notice>}
    <section className="section-block"><div className="section-heading"><span>CONTACTS</span><span>{book.cards.length}</span></div>
      {book.cards.length === 0 ? <Empty what="This book is empty." next="Create a Card, or stage a card-exchange file." /> : <div className="card-grid">{book.cards.map((card) => <button className="contact-card" key={card.card} onClick={() => onEdit(card.card)}><Avatar card={card} /><span><strong>{card.name}</strong><small>{presenceLabel(card.presence)} · {card.handles.length} handle{card.handles.length === 1 ? "" : "s"}</small></span>{card.selfClaim && <em>ME</em>}</button>)}</div>}
    </section>
    <section className="inline-form section-block"><div><span className="section-heading">CARD EXCHANGE</span><p>Stage an exchange file for review; it does not write into the book automatically.</p></div><input value={path} onChange={(event) => setPath(event.target.value)} placeholder="/path/to/cards.json" /><button className="quiet-button" disabled={path.trim() === ""} onClick={() => void dispatch({ type: "bookImport", path: path.trim() })}>Stage import</button></section>
    <section className="inline-form section-block"><div><span className="section-heading">EXPORT</span><p>Write this book to a chosen local path.</p></div><input value={exportPath} onChange={(event) => setExportPath(event.target.value)} placeholder="/path/to/export.json" /><button className="quiet-button" disabled={exportPath.trim() === ""} onClick={() => void dispatch({ type: "bookExport", path: exportPath.trim(), cards: null })}>Export all</button></section>
    {book.suggestions.length > 0 && <section className="section-block"><div className="section-heading"><span>REVIEW</span><span>{book.suggestions.length}</span></div>{book.suggestions.map((suggestion) => <div className="review-card" key={suggestion.suggestion}><div><strong>{suggestion.name}</strong><p>{suggestion.note || "No note"}</p><small>{suggestion.handles.join(" · ") || "No handles"}</small></div><div className="button-row"><button className="quiet-button" onClick={() => void dispatch({ type: "bookDismiss", suggestion: suggestion.suggestion })}>Dismiss</button><button className="primary-button" onClick={() => void dispatch({ type: "bookAccept", suggestion: suggestion.suggestion })}>Accept</button></div></div>)}</section>}
  </div>;
}

function BookEditor({ card, all, view, dispatch, onBack }: { card: Card | undefined; all: Card[]; view: ClientView; dispatch(action: ClientAction): Promise<void>; onBack(): void }) {
  const [name, setName] = useState(card?.name ?? "");
  const [note, setNote] = useState(card?.note ?? "");
  const [handle, setHandle] = useState("");
  const [picturePath, setPicturePath] = useState("");
  const [mergeInto, setMergeInto] = useState("");
  const key = card === undefined ? actionKey.bookPut(null) : actionKey.bookPut(card.card);
  const saving = view.inFlight.includes(key);
  const save = () => void dispatch({ type: "bookPut", card: card?.card ?? null, name: name.trim(), note: note.trim() || null });
  return <SecondaryFrame title={card === undefined ? "New card" : card.name} detail={card === undefined ? "Author a Card for an identity." : "Edit this authored Card."} onBack={onBack}
    actions={<button className="primary-button" disabled={name.trim() === "" || saving} onClick={save}>{saving ? "Saving…" : "Save"}</button>}>
    <div className="secondary-scroll editor-surface"><label>Name<input value={name} onChange={(event) => setName(event.target.value)} autoFocus /></label><label>Note<textarea value={note} onChange={(event) => setNote(event.target.value)} rows={3} /></label>
      {card !== undefined && <>
        <section className="section-block"><div className="section-heading"><span>HANDLES</span><span>{card.handles.length}</span></div>{card.handles.length === 0 ? <Empty what="No handles are linked." /> : <div className="token-list">{card.handles.map((item) => <span className="token" key={item}>{item}<button aria-label={`Unlink ${item}`} onClick={() => void dispatch({ type: "bookUnlink", card: card.card, handle: item })}>×</button></span>)}</div>}<div className="inline-form"><input value={handle} onChange={(event) => setHandle(event.target.value)} placeholder="actor:… or device id" /><button className="quiet-button" disabled={handle.trim() === ""} onClick={() => { void dispatch({ type: "bookLink", card: card.card, handle: handle.trim() }); setHandle(""); }}>Link</button></div></section>
        <section className="section-block"><div className="section-heading">PICTURE</div><div className="inline-form"><input value={picturePath} onChange={(event) => setPicturePath(event.target.value)} placeholder="/path/to/picture.png" /><button className="quiet-button" disabled={picturePath.trim() === ""} onClick={() => void dispatch({ type: "bookSetPicture", card: card.card, path: picturePath.trim() })}>Set picture</button>{card.picture !== null && <button className="danger-button" onClick={() => void dispatch({ type: "bookSetPicture", card: card.card, path: null })}>Clear</button>}</div></section>
        <section className="section-block button-row"><button className="quiet-button" disabled={card.selfClaim} onClick={() => void dispatch({ type: "bookClaimSelf", card: card.card })}>{card.selfClaim ? "My Card" : "Claim as My Card"}</button><select value={mergeInto} onChange={(event) => setMergeInto(event.target.value)}><option value="">Merge into…</option>{all.filter((item) => item.card !== card.card).map((item) => <option key={item.card} value={item.card}>{item.name}</option>)}</select><button className="danger-button" disabled={mergeInto === ""} onClick={() => void dispatch({ type: "bookMerge", from: card.card, into: mergeInto })}>Merge</button><button className="danger-button" onClick={() => { if (window.confirm(`Delete ${card.name}?`)) void dispatch({ type: "bookDelete", card: card.card }); }}>Delete</button></section>
      </>}
    </div>
  </SecondaryFrame>;
}

export function DisplaysSurface({ view, dispatch, onBack }: SurfaceProps) {
  const display = view.display;
  return <SecondaryFrame title="Displays" detail="Enroll receivers and pin each one to an exact World surface." onBack={onBack} actions={<button className="quiet-button" onClick={() => void dispatch({ type: "refresh" })}>Refresh</button>}>
    {display === null ? <Unread what="The display coordinator has not answered yet." /> : <DisplaysBody display={display} view={view} dispatch={dispatch} />}
  </SecondaryFrame>;
}

function DisplaysBody({ display, view, dispatch }: { display: Display; view: ClientView; dispatch(action: ClientAction): Promise<void> }) {
  return <div className="secondary-scroll displays-surface"><section className="coordinator-card"><span className="eyebrow">COORDINATOR</span><strong>{display.label}</strong><code>{display.origin}</code><small>Certificate {shortId(display.certificateSha256)}</small></section>
    <section className="section-block"><div className="section-heading"><span>PAIRING REQUESTS</span><span>{display.pendingPairings.length}</span></div>{display.pendingPairings.length === 0 ? <Empty what="No receiver is waiting for approval." next="Begin pairing from a display receiver." /> : display.pendingPairings.map((pairing) => <PairingCard key={pairing.pairing} pairing={pairing} dispatch={dispatch} />)}</section>
    <section className="section-block"><div className="section-heading"><span>RECEIVERS</span><span>{display.devices.length}</span></div>{display.devices.length === 0 ? <Empty what="No receiver is enrolled." /> : display.devices.map((receiver) => <ReceiverCard key={receiver.device} receiver={receiver} assignment={display.assignments.find((item) => item.device === receiver.device && item.revokedAtUnixMs === null)} display={display} view={view} dispatch={dispatch} />)}</section>
  </div>;
}

function PairingCard({ pairing, dispatch }: { pairing: Display["pendingPairings"][number]; dispatch(action: ClientAction): Promise<void> }) {
  const [label, setLabel] = useState("");
  return <div className="review-card"><div><strong>{pairing.platform} receiver</strong><p className="phrase">{pairing.confirmationPhrase.join(" ")}</p><small>{shortId(pairing.certificateSha256)} · {pairing.build}</small></div><div className="button-stack"><input value={label} onChange={(event) => setLabel(event.target.value)} placeholder="Receiver label" /><div className="button-row"><button className="quiet-button" onClick={() => void dispatch({ type: "displayPairingReject", pairing: pairing.pairing })}>Reject</button><button className="primary-button" disabled={label.trim() === ""} onClick={() => void dispatch({ type: "displayPairingApprove", pairing: pairing.pairing, label: label.trim() })}>Approve</button></div></div></div>;
}

function ReceiverCard({ receiver, assignment, display, view, dispatch }: { receiver: Display["devices"][number]; assignment: Display["assignments"][number] | undefined; display: Display; view: ClientView; dispatch(action: ClientAction): Promise<void> }) {
  const [assigning, setAssigning] = useState(false);
  return <article className="receiver-card"><div className="receiver-title"><div><strong>{receiver.label}</strong><small>{receiver.platform} · {receiver.build} · {shortId(receiver.device)}</small></div><span className={receiver.revokedAtUnixMs === null ? "status-pill good" : "status-pill"}>{receiver.revokedAtUnixMs === null ? receiver.health?.connection ?? "enrolled" : "revoked"}</span></div>
    {receiver.health !== null && <p className="health-line">{receiver.health.playback || "Idle"} · {receiver.health.currentItem || "No current item"} · {receiver.health.stagedItems} staged</p>}
    {assignment === undefined ? <>{assigning ? <AssignmentForm device={receiver.device} display={display} view={view} dispatch={dispatch} done={() => setAssigning(false)} /> : <button className="quiet-button" disabled={receiver.revokedAtUnixMs !== null} onClick={() => setAssigning(true)}>Assign surface</button>}</> : <div className="assignment-line"><span><strong>{assignment.world}/{assignment.surface}</strong><small>{assignment.orbit} · {assignment.theme}</small></span><button className="danger-button" onClick={() => void dispatch({ type: "displayAssignmentRevoke", assignment: assignment.assignment })}>Revoke assignment</button></div>}
    <button className="text-button danger-text" onClick={() => { if (window.confirm(`Revoke ${receiver.label}?`)) void dispatch({ type: "displayDeviceRevoke", device: receiver.device }); }}>Revoke receiver</button>
  </article>;
}

function AssignmentForm({ device, display, view, dispatch, done }: { device: string; display: Display; view: ClientView; dispatch(action: ClientAction): Promise<void>; done(): void }) {
  const first = display.surfaces[0];
  const [surfaceKey, setSurfaceKey] = useState(first === undefined ? "" : `${first.world}:${first.surface}`);
  const [orbit, setOrbit] = useState(view.orbits[0]?.space ?? "");
  const [inputJson, setInputJson] = useState("{}");
  const selected = display.surfaces.find((item) => `${item.world}:${item.surface}` === surfaceKey);
  return <div className="assignment-form"><select value={surfaceKey} onChange={(event) => setSurfaceKey(event.target.value)}><option value="">Choose surface…</option>{display.surfaces.map((item) => <option key={`${item.world}:${item.surface}`} value={`${item.world}:${item.surface}`}>{item.title} · {item.world}/{item.surface}</option>)}</select><select value={orbit} onChange={(event) => setOrbit(event.target.value)}><option value="">Choose Space…</option>{view.orbits.map((item) => <option key={item.space} value={item.space}>{item.name}</option>)}</select><textarea value={inputJson} onChange={(event) => setInputJson(event.target.value)} rows={2} aria-label="Surface input JSON" /><div className="button-row"><button className="quiet-button" onClick={done}>Cancel</button><button className="primary-button" disabled={selected === undefined || orbit === ""} onClick={() => { void dispatch({ type: "displayAssignmentPut", device, orbit, world: selected!.world, surface: selected!.surface, inputJson, theme: "dark", staleAfterMs: 60000, onStale: "keepWithNativeBanner", syncGroup: null, syncMode: "stayInSync", staticDelayMs: 0, expiresAtUnixMs: null }); done(); }}>Assign</button></div></div>;
}

export function RecordSurface({ view, dispatch, onBack }: SurfaceProps) {
  return <SecondaryFrame title="Local record" detail="Supervised devices, browser heads, Spaces, and authored MCP bindings." onBack={onBack} actions={<button className="quiet-button" onClick={() => void dispatch({ type: "refresh" })}>Refresh</button>}>
    <div className="secondary-scroll record-surface"><section className="section-block"><div className="section-heading"><span>DEVICES</span><span>{view.devices.length}</span></div><div className="record-grid">{view.devices.map((device) => <DeviceCard key={device.id} device={device} dispatch={dispatch} />)}</div>{view.devices.some((device) => device.owned) && <button className="danger-button" onClick={() => { if (window.confirm("Stop every device this client owns?")) void dispatch({ type: "stopAllOwned" }); }}>Stop all owned devices</button>}</section>
      <section className="section-block"><div className="section-heading"><span>HEADS</span><span>{view.heads.length}</span></div><div className="button-row"><button className="primary-button" onClick={() => void dispatch({ type: "startHead" })}>Start browser head</button>{view.heads.map((head) => <span className="token" key={head.id}>{head.kind} · {shortId(head.id)}{head.owned && <button onClick={() => void dispatch({ type: "stopHead", id: head.id })}>×</button>}</span>)}</div></section>
      <section className="section-block"><div className="section-heading"><span>SPACES</span><span>{view.orbits.length}</span></div>{view.orbits.map((orbit) => <div className="orbit-row" key={orbit.space}><span><strong>{orbit.name}</strong><small>{shortId(orbit.space)} · {orbit.path}</small></span><div className="button-row"><button className="quiet-button" onClick={() => void dispatch({ type: "readSpace", orbit: orbit.space })}>Inspect</button><button className="danger-button" onClick={() => { if (window.confirm(`Forget ${orbit.name}? Its store remains on disk.`)) void dispatch({ type: "forgetOrbit", space: orbit.space }); }}>Forget</button></div></div>)}{view.space !== null && <SpaceFacts view={view} />}</section>
      <McpPanel binding={view.mcp} worlds={view.library ?? []} dispatch={dispatch} />
    </div>
  </SecondaryFrame>;
}

function DeviceCard({ device, dispatch }: { device: ClientView["devices"][number]; dispatch(action: ClientAction): Promise<void> }) {
  return <article className="device-card"><strong>{device.label}</strong><small>{device.state} · {shortId(device.id)}</small>{device.degraded !== null && <Notice tone="warn">{device.degraded}</Notice>}{device.lastError !== null && <Notice tone="danger">{device.lastError}</Notice>}<div className="button-row">{device.owned ? <><button className="quiet-button" onClick={() => void dispatch({ type: "stopDevice", id: device.id })}>Stop</button><button className="quiet-button" onClick={() => void dispatch({ type: "restartDevice", id: device.id })}>Restart</button>{device.canForceStop && <button className="danger-button" onClick={() => void dispatch({ type: "forceStopDevice", id: device.id })}>Force stop</button>}</> : <span className="muted">Not owned by this client</span>}<button className="text-button danger-text" onClick={() => { if (window.confirm(`Remove ${device.label}?`)) void dispatch({ type: "removeDevice", id: device.id, deleteData: false }); }}>Remove</button></div></article>;
}

function SpaceFacts({ view }: { view: ClientView }) {
  const space = view.space!;
  return <div className="space-facts"><strong>{space.whoami ?? "No local standing"}</strong><p>{space.diagnosis?.summary ?? "No diagnosis has been read."}</p>{space.diagnosis?.gates.map((gate) => <span className={`gate gate-${gate.state}`} key={gate.id}>{gate.label}: {gate.detail}</span>)}<div className="member-list">{space.members.map((member) => <span key={member.id}>{member.authoredName ?? member.nick ?? shortId(member.id)}{member.admin ? " · admin" : ""}</span>)}</div></div>;
}

function McpPanel({ binding, worlds, dispatch }: { binding: McpBinding | null; worlds: NonNullable<ClientView["library"]>; dispatch(action: ClientAction): Promise<void> }) {
  const [client, setClient] = useState("claude"); const [name, setName] = useState("lait"); const [project, setProject] = useState(""); const [world, setWorld] = useState("");
  return <section className="section-block"><div className="section-heading">MCP BINDING</div>{binding !== null && <Notice tone="good">{binding.written ? "Written" : "Preview"}: {binding.detail}</Notice>}<div className="mcp-form"><select value={client} onChange={(event) => setClient(event.target.value)}><option value="claude">Claude</option><option value="cursor">Cursor</option><option value="windsurf">Windsurf</option><option value="generic">Generic</option></select><input value={name} onChange={(event) => setName(event.target.value)} placeholder="Binding name" /><input value={project} onChange={(event) => setProject(event.target.value)} placeholder="Project path" /><select value={world} onChange={(event) => setWorld(event.target.value)}><option value="">Default World</option>{worlds.map((item) => <option key={item.worldMount} value={item.worldMount}>{item.displayName}</option>)}</select><div className="button-row"><button className="quiet-button" disabled={name.trim() === "" || project.trim() === ""} onClick={() => void dispatch({ type: "installMcp", client, scope: null, name: name.trim(), agent: null, noAgent: false, project: project.trim(), world: world || null, preview: true })}>Preview</button><button className="primary-button" disabled={name.trim() === "" || project.trim() === ""} onClick={() => void dispatch({ type: "installMcp", client, scope: null, name: name.trim(), agent: null, noAgent: false, project: project.trim(), world: world || null, preview: false })}>Write binding</button></div></div></section>;
}

function SecondaryFrame({ title, detail, onBack, actions, children }: { title: string; detail: string; onBack(): void; actions?: ReactNode; children: ReactNode }) { return <section className="secondary-surface"><header className="secondary-header"><button className="back-button" onClick={onBack}>← Library</button><div><h1>{title}</h1><p>{detail}</p></div><div className="header-actions">{actions}</div></header>{children}</section>; }
function Unread({ what }: { what: string }) { return <div className="secondary-scroll"><Empty what={what} next="Refresh local state to ask the daemon." /></div>; }
function Empty({ what, next }: { what: string; next?: string }) { return <div className="empty-state"><strong>{what}</strong>{next !== undefined && <p>{next}</p>}</div>; }
function Notice({ tone, children }: { tone: "warn" | "danger" | "good"; children: ReactNode }) { return <p className={`notice notice-${tone}`}>{children}</p>; }
function Avatar({ card }: { card: Card }) { return card.picture === null ? <span className="avatar">{card.name.slice(0, 1).toUpperCase()}</span> : <img className="avatar" src={card.picture} alt="" />; }
function presenceLabel(presence: Card["presence"]) { return presence === null ? "unmeasured" : presence; }
function shortId(value: string) { return value.length > 18 ? `${value.slice(0, 8)}…${value.slice(-6)}` : value; }
