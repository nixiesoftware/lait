/**
 * The Hub — your central place in the client, opened from the caption bar's
 * identity button. It replaces the Library in the window rather than opening
 * a window of its own: managing yourself is a first-class view, not a popup.
 *
 * Everything here is a projection of facts other surfaces already own — the
 * claimed card, the friend code, the agent cards, the device set — gathered
 * where "me" is the subject. Dispatch is the only write, and the dialogs are
 * the book's own, so a name edited here is the same edit the book makes.
 */
import { useState } from "react";

import { actionKey, summonOwnedWindow, type Card, type ClientAction, type ClientView } from "./client";
import { AddDialog, AgentBand, EditDialog, PictureDialog } from "./book";
import { FacePlate, isAgentCard, presenceLabel } from "./kit";
import { IconBook, IconDevices } from "./icons";

type Dispatch = (action: ClientAction) => Promise<void>;

type HubDialog =
  | { kind: "add" }
  | { kind: "edit"; card: Card }
  | { kind: "picture"; card: Card };

export function HubSurface({ view, dispatch, onBack }: {
  view: ClientView; dispatch: Dispatch; onBack(): void;
}) {
  const [dialog, setDialog] = useState<HubDialog | null>(null);
  const [copied, setCopied] = useState(false);
  const mine = view.book?.cards.find((card) => card.selfClaim) ?? null;
  const agents = view.book?.cards.filter(isAgentCard) ?? [];
  const code = view.correspondence?.myAddress ?? null;
  const sharing = view.inFlight.includes(actionKey.shareReach);
  const status = mine === null
    ? null
    : presenceLabel(mine.presence) ?? (view.host !== null ? "Online" : null);

  return <section className="hub" aria-label="Your hub">
    <div className="hub-inner">
      <div><button className="back-button" onClick={onBack}>← Library</button></div>

      <div className="hub-head">
        <FacePlate picture={mine?.picture ?? null} name={mine?.name ?? ""} size={88} />
        <span className="person-copy">
          <strong className="hub-name">{mine?.name ?? "Your card"}</strong>
          {mine === null
            ? <small>How you appear to others — set it up to claim a name and a face.</small>
            : <>
              {status !== null && <small>{status}</small>}
              {mine.note !== "" && <small>{mine.note}</small>}
            </>}
        </span>
      </div>
      <div className="button-row">
        {mine === null
          ? <button className="primary-button" onClick={() => setDialog({ kind: "add" })}>Set up your card</button>
          : <>
            <button className="quiet-button" onClick={() => setDialog({ kind: "edit", card: mine })}>Edit</button>
            <button className="quiet-button" onClick={() => setDialog({ kind: "picture", card: mine })}>Set picture</button>
          </>}
      </div>

      <section className="hub-section">
        <span className="fact-label">FRIEND CODE</span>
        {code !== null
          ? <div className="friend-code-band">
            <code className="friend-code">{code}</code>
            <button className="quiet-button" onClick={() => {
              void navigator.clipboard.writeText(code);
              setCopied(true);
              setTimeout(() => setCopied(false), 1600);
            }}>{copied ? "Copied" : "Copy"}</button>
          </div>
          : <div className="friend-code-band">
            <span className="muted">Publishing makes you reachable by a short spoken code.</span>
            <button className="quiet-button" disabled={sharing}
              onClick={() => void dispatch({ type: "shareReach" })}>{sharing ? "Publishing…" : "Publish"}</button>
          </div>}
      </section>

      {agents.length > 0 && <section className="hub-section">
        <AgentBand agents={agents} onOpen={(card) => {
          const agent = agents.find((row) => row.card === card);
          if (agent !== undefined) setDialog({ kind: "edit", card: agent });
        }} />
      </section>}

      <section className="hub-section">
        <span className="fact-label">THIS IDENTITY</span>
        <div className="hub-links">
          <button className="quiet-button" onClick={() => void summonOwnedWindow("devices")}>
            <IconDevices size={16} /> {view.devices.length === 1 ? "1 device" : `${view.devices.length} devices`}
          </button>
          <button className="quiet-button" onClick={() => void summonOwnedWindow("book")}>
            <IconBook size={16} /> Address book
          </button>
        </div>
      </section>
    </div>

    {dialog?.kind === "add" && <AddDialog dispatch={dispatch} onDismiss={() => setDialog(null)} />}
    {dialog?.kind === "edit" && <EditDialog card={dialog.card} dispatch={dispatch} onDismiss={() => setDialog(null)} />}
    {dialog?.kind === "picture" && <PictureDialog card={dialog.card} dispatch={dispatch} onDismiss={() => setDialog(null)} />}
  </section>;
}
