import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Bot,
  Check,
  Copy,
  KeyRound,
  Link2,
  Pencil,
  ShieldAlert,
  ShieldCheck,
  UserPlus,
  X,
} from "lucide-react";

import { ConfirmRequired, hostRpc, spaceRpc as rpc } from "../api";
import type { MemberDto, MemberLogEntry } from "../types";
import { Avatar, memberName } from "./Avatar";
import * as ask from "./dialogs";
import { Combobox } from "./Picker";
import { Button, IconButton, TextInput } from "@astryxdesign/core";
import { ApplicationState, EmptyState, InlineError, LoadingState } from "./AppState";
import { Badge } from "./primitives";
import { SettingsPageHeader } from "./settingsLayout";

/**
 * Members and the invite link. Admission needs no controls here: accepting an
 * invite IS the approval, and redemption is automatic on the next contact.
 *
 * Roles come from the signed ACL graph — the only cryptographically-verified
 * identity in the system. Admin-only actions are hidden for non-admins rather than
 * offered and rejected.
 */
export function Members({
  spaceId,
  revision,
  readOnly,
  onError,
  embedded = false,
}: {
  spaceId: string;
  revision: number;
  readOnly: boolean;
  onError: (m: string) => void;
  /** Settings owns scrolling and content width when Members is a tab. */
  embedded?: boolean;
}) {
  const [members, setMembers] = useState<MemberDto[] | null>(null);
  /**
   * Why there is no roster, when there is none.
   *
   * `members === null` meant "not loaded", and the catch below returned without
   * setting it — so a failed read drew "Loading members" forever, a spinner
   * promising progress that was never coming.
   */
  const [failure, setFailure] = useState<string | null>(null);
  const [log, setLog] = useState<MemberLogEntry[]>([]);
  const [logError, setLogError] = useState("");
  const [ticket, setTicket] = useState<string | null>(null);
  const [inviting, setInviting] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [query, setQuery] = useState("");

  const load = useCallback(async () => {
    try {
      const m = await rpc(spaceId, { cmd: "members" });
      if (m.kind === "members") setMembers(m.members);
      setFailure(null);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      // The pane owns it when there is no roster to keep showing; otherwise the
      // roster stays and the transient notice says the refresh did not land.
      setFailure(message);
      if (members) onError(message);
      return;
    }
    try {
      const l = await rpc(spaceId, { cmd: "member_log" });
      if (l.kind === "member_log") setLog(l.entries);
    } catch {
      // The audit log is a nicety, not load-bearing for the roster; a failure just
      // hides the section rather than breaking the page.
      setLog([]);
      setLogError("The access log is temporarily unavailable. The member roster is still current.");
    }
  }, [spaceId, onError]);

  useEffect(() => {
    void load();
  }, [load, revision]);

  const isAdmin = members?.some((m) => m.me && m.role === "admin") ?? false;
  const shownMembers = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return members ?? [];
    return (members ?? []).filter((member) =>
      [member.alias, member.did, member.key, member.role]
        .filter(Boolean)
        .some((value) => value!.toLowerCase().includes(needle)),
    );
  }, [members, query]);

  /** The busy key for the one action that belongs to no row. */
  const SPONSOR = "agent:new";

  const act = async (id: string, fn: () => Promise<unknown>) => {
    setBusy(id);
    try {
      await fn();
      await load();
    } catch (e) {
      // A destructive verb comes back as the CLI's own question; if the human
      // says no, that is an answer, not a failure.
      if (!(e instanceof ConfirmRequired)) {
        onError(e instanceof Error ? e.message : String(e));
      }
    } finally {
      setBusy(null);
    }
  };

  if (!members) {
    if (failure) {
      return (
        <ApplicationState
          kind="retry"
          art="unavailable"
          title="Members are unavailable"
          body={failure}
          action={<Button onClick={() => void load()} label="Retry" variant="ghost" size="sm" />}
        />
      );
    }
    return <LoadingState title="Loading members" body="Verifying this space’s signed access graph." />;
  }

  const people = shownMembers.filter((m) => !m.sponsor);
  const agents = shownMembers.filter((m) => Boolean(m.sponsor));
  const grid = "grid grid-cols-[minmax(0,1.4fr)_minmax(0,1fr)_10rem_4.5rem] items-center gap-4";

  return (
    <div className={embedded ? undefined : "min-h-0 flex-1 overflow-y-auto"}>
      <div className={embedded ? "flex flex-col gap-6" : "mx-auto flex max-w-4xl flex-col gap-6 p-6"}>
        <SettingsPageHeader
          title="Members"
          description="People and agents with verified access to this encrypted space. Names are private labels on this device."
          className="mb-0"
          actions={
            !readOnly ? (
              <>
                <Button
                  isDisabled={busy === SPONSOR}
                  onClick={() =>
                    void act(SPONSOR, async () => {
                      const name = await ask.prompt({
                        title: "Sponsor an agent",
                        body: "An agent is a member with its own key. Sponsoring one mints that key on this machine and gives it write access, so its work is signed as itself and never as you. Its standing ends when yours does. This is the local name its identity is kept under, and the one an MCP client passes as LAIT_AGENT.",
                        label: "Name",
                        defaultValue: "claude",
                      });
                      if (!name?.trim()) return;
                      await rpc(spaceId, { cmd: "agent_provision", name: name.trim() });
                    })
                  }
                  icon={<Bot className="size-icon-sm" />}
                  label="Sponsor an agent"
                  variant="secondary"
                  size="sm"
                />
                {isAdmin && (
                  <Button
                    label="Invite"
                    variant="primary"
                    size="sm"
                    icon={<UserPlus className="size-icon-sm" />}
                    aria-expanded={inviting}
                    onClick={() => setInviting((open) => !open)}
                  />
                )}
              </>
            ) : undefined
          }
        />

        {isAdmin && !readOnly && inviting && (
          <Invite spaceId={spaceId} ticket={ticket} setTicket={setTicket} onError={onError} />
        )}

        {members.length > 0 && (
          <div className="flex items-center gap-3">
            <div className="w-full max-w-xs">
              <TextInput
                label="Filter members"
                isLabelHidden
                value={query}
                onChange={setQuery}
                placeholder="Search by name or identity…"
                size="sm"
                width="100%"
              />
            </div>
            <span className="text-mute ml-auto text-xs tabular-nums">
              {members.length} {members.length === 1 ? "member" : "members"}
            </span>
          </div>
        )}

        <section>
          {members.length === 0 ? (
            <EmptyState art="people" title="Members" body="People and agents with verified access to this space appear here." />
          ) : (
            <div className="border-line overflow-hidden rounded-surface border">
              <div className={`${grid} text-mute border-line border-b px-3 py-2 text-2xs`}>
                <span>Name</span>
                <span>Identity</span>
                <span>Role</span>
                <span aria-hidden />
              </div>
              {shownMembers.length === 0 && (
                <p className="text-mute px-3 py-6 text-center text-sm">Nothing matches “{query}”.</p>
              )}
              {(
                [
                  ["People", people],
                  ["Agents", agents],
                ] as const
              )
                .filter(([, rows]) => rows.length > 0)
                .map(([heading, rows]) => (
                  <div key={heading}>
                    <div className="bg-sunken text-mute flex items-center gap-2 px-3 py-1.5 text-2xs">
                      <span className="font-semibold tracking-wider uppercase">{heading}</span>
                      <span className="tabular-nums">{rows.length}</span>
                    </div>
                    <ul className="divide-line divide-y">
                      {rows.map((m) => (
                        <li key={m.key} className={`${grid} group/member hover:bg-hover min-h-ctl-xl px-3 py-2`}>
                          <span className="flex min-w-0 items-center gap-2.5">
                            <Avatar deviceKey={m.key} alias={m.alias} me={m.me} className="size-avatar-lg" />
                            <span className="min-w-0">
                              <span className="block truncate text-sm font-medium">
                                {m.alias || <span className="text-mute italic">unnamed</span>}
                                {m.me && <span className="text-mute ml-1.5 text-2xs font-normal">you</span>}
                              </span>
                              <span className="text-mute block truncate text-xs">
                                {m.sponsor
                                  ? `Sponsored by ${sponsorName(m, members)}`
                                  : m.me
                                    ? "This device"
                                    : "Member"}
                              </span>
                            </span>
                          </span>
                          <code className="text-mute truncate font-mono text-xs" title={m.did ?? m.key}>
                            {m.did ?? m.key}
                          </code>
                          <span>
                            {m.role === "admin" ? (
                              <Badge tone="accent" className="rounded-mark border-0">
                                <ShieldCheck className="size-icon-xs" />
                                Admin
                              </Badge>
                            ) : (
                              <Badge tone="neutral" className="rounded-mark border-0">
                                {roleLabel(m.role)}
                              </Badge>
                            )}
                          </span>
                          <span className="flex justify-end gap-0.5 opacity-0 group-hover/member:opacity-100 focus-within:opacity-100">
                            {isAdmin && !readOnly && (
                              <>
                                <IconButton
                                  label="Name in your address book"
                                  isDisabled={busy === m.key}
                                  onClick={() =>
                                    void act(m.key, async () => {
                                      const name = await ask.prompt({
                                        title: "Name in your address book",
                                        body: "Authors a Card in your identity's book — private to you, never synced. Manage Cards in the address book.",
                                        label: "Name",
                                        defaultValue: m.alias,
                                      });
                                      if (name === null || !name.trim()) return;
                                      // The book is the one namer: find the Card this
                                      // member's handle is on, or author one and link it.
                                      const who = await rpc(spaceId, { cmd: "whoami" });
                                      if (who.kind !== "whoami" || !who.space) return;
                                      const handle = `actor:${who.space}:${m.key}`;
                                      const found = await hostRpc({ cmd: "book_lookup", handle });
                                      const existing =
                                        found.kind === "book" ? found.cards[0]?.card : undefined;
                                      if (existing) {
                                        await hostRpc({ cmd: "book_put", card: existing, name: name.trim() });
                                      } else {
                                        const put = await hostRpc({ cmd: "book_put", name: name.trim() });
                                        const card =
                                          put.kind === "book"
                                            ? put.cards.find(
                                                (c) => c.name === name.trim() && c.handles.length === 0,
                                              )?.card
                                            : undefined;
                                        if (card) await hostRpc({ cmd: "book_link", card, handle });
                                      }
                                    })
                                  }
                                  variant="ghost"
                                  size="sm"
                                  tooltip="Name in your address book"
                                  icon={<Pencil className="size-icon-sm" />}
                                />
                                {!m.me && (
                                  <IconButton
                                    label="Remove (rotates the space key)"
                                    tooltip="Remove (rotates the space key)"
                                    variant="ghost"
                                    size="sm"
                                    icon={<X className="size-icon-sm" />}
                                    isDisabled={busy === m.key}
                                    onClick={() =>
                                      void act(m.key, async () => {
                                        try {
                                          await rpc(spaceId, { cmd: "member_remove", who: m.key });
                                        } catch (e) {
                                          // The engine hands back its own question — removing
                                          // rotates the space key, and it says so.
                                          if (e instanceof ConfirmRequired) {
                                            if (
                                              await ask.confirm({
                                                title: `Remove ${memberName(m.key, m)} from this space?`,
                                                body: `${e.question} They will lose future access and the space encryption key will rotate. This does not erase copies they already received.`,
                                                confirmText: "Remove",
                                                danger: true,
                                              })
                                            ) {
                                              await rpc(
                                                spaceId,
                                                { cmd: "member_remove", who: m.key },
                                                { confirm: true },
                                              );
                                            }
                                            return;
                                          }
                                          throw e;
                                        }
                                      })
                                    }
                                  />
                                )}
                              </>
                            )}
                          </span>
                        </li>
                      ))}
                    </ul>
                  </div>
                ))}
            </div>
          )}
        </section>

        {logError && <InlineError message={logError} onRetry={() => void load()} />}
        {log.length > 0 && <MemberLog entries={log} members={members} />}
      </div>
    </div>
  );
}

function roleLabel(role: string): string {
  return (
    { viewer: "Viewer", contributor: "Contributor", member: "Member", administrator: "Admin" } as Record<
      string,
      string
    >
  )[role] ?? role;
}

/** The display name of an agent's sponsor, resolved through the same local
 * petname rule as everyone else (falls back to a short key). Renders the
 * sponsor relationship as information — "the surface accounts for agents". */
function sponsorName(m: MemberDto, members: MemberDto[]): string {
  if (!m.sponsor) return "";
  return memberName(m.sponsor, members.find((x) => x.key === m.sponsor));
}

/**
 * The membership audit log.
 *
 * The one feed in lait whose author is **cryptographically verified**: `actor`
 * signed the op, and the signature covers it (unlike in-doc activity, which is
 * advisory). That is why it belongs here and not in the activity feed — it is the
 * record of who changed *access*, which is exactly the thing you want to be sure of.
 *
 * An `authorized: false` row is shown, not hidden. A rejected op is a real event —
 * someone tried something the ACL refused — and quietly dropping it would defeat the
 * point of having an audit trail at all.
 */
function MemberLog({ entries, members }: { entries: MemberLogEntry[]; members: MemberDto[] }) {
  const name = (key: string) => memberName(key, members.find((m) => m.key === key));
  const PHRASE: Record<string, string> = {
    add_member: "added",
    remove_member: "removed",
    set_role: "set the role of",
    add_agent: "sponsored agent",
    grant_capability: "granted an access capability",
    activate_implementation: "activated a new access policy",
    mint_epoch: "rotated the space encryption key",
    unknown: "(unrecognized op)",
  };

  return (
    <details className="border-line overflow-hidden rounded-surface border">
      <summary className="hover:bg-hover cursor-pointer px-3 py-2.5 text-base font-semibold">
        Access log · {entries.length}
      </summary>
      <ul className="border-line divide-line divide-y border-t">
        {/* Newest first — an audit log answers "what just changed access". */}
        {[...entries].reverse().map((e) => (
          <li key={e.op} className="flex items-start gap-2 p-2.5 text-sm">
            <span className="min-w-0 flex-1">
              <span className="font-medium">{name(e.actor)}</span>{" "}
              <span className="text-dim">{PHRASE[e.kind] ?? e.kind}</span>
              {e.subject && <span className="font-medium"> {name(e.subject)}</span>}
              {e.role && <span className="text-mute"> as {e.role}</span>}
              <details className="text-mute mt-1 text-xs">
                <summary className="w-fit cursor-default">Technical details</summary>
                <code className="mt-1 block break-all">Signed operation {e.op}</code>
                <span>{e.authorized ? "Signature verified and the access rule accepted this change." : "The access rule rejected or could not decode this operation."}</span>
              </details>
            </span>
            {!e.authorized && (
              <span
                className="text-danger flex items-center gap-1 text-2xs"
                title="Replay rejected this op as unauthorized or undecodable"
              >
                <ShieldAlert className="size-icon-xs" />
                rejected
              </span>
            )}
          </li>
        ))}
      </ul>
    </details>
  );
}

/** The roles an invite can admit as — `cli::invite`'s exact vocabulary. */
const INVITE_ROLES = [
  { id: "contributor", label: "Contributor", hint: "read + write issues" },
  { id: "viewer", label: "Viewer", hint: "read-only" },
  { id: "administrator", label: "Administrator", hint: "full control" },
] as const;

/** Expiry choices, in the engine's unit (hours). 168 is the daemon's default. */
const INVITE_TTLS = [
  { hours: 24, label: "1 day" },
  { hours: 168, label: "7 days" },
  { hours: 720, label: "30 days" },
] as const;

/**
 * The invite surface.
 *
 * `Request::Invite` returns the bare **link body**; the `lait://join/…` link is
 * derived from it here. The capability always auto-admits — the joiner pastes
 * that link into Welcome's **Use an invite**, which sends `host_space_enter`,
 * and they are in; accepting the invite is the approval. The options are the
 * capability's own knobs: the admitted role rides in the signed evidence,
 * `reusable` admits a whole team until expiry, and the TTL bounds how long the
 * link can admit anyone.
 */
function Invite({
  spaceId,
  ticket,
  setTicket,
  onError,
}: {
  spaceId: string;
  ticket: string | null;
  setTicket: (t: string | null) => void;
  onError: (m: string) => void;
}) {
  const [qr, setQr] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [role, setRole] = useState<string>("contributor");
  const [reusable, setReusable] = useState(false);
  const [ttl, setTtl] = useState<number>(168);

  const link = ticket ? `lait://join/${ticket}` : null;

  useEffect(() => {
    if (!link) return setQr(null);
    // Rendered locally. A remote QR service would mean handing an invite ticket —
    // which admits someone to an E2EE space — to a third party.
    void import("qrcode")
      .then(({ default: QRCode }) =>
        QRCode.toDataURL(link, { margin: 1, width: 220, errorCorrectionLevel: "L" }),
      )
      .then(setQr)
      .catch(() => setQr(null));
  }, [link]);

  const mint = async () => {
    try {
      const r = await rpc(spaceId, { cmd: "invite", role, reusable, ttl_hours: ttl });
      if (r.kind === "ref") setTicket(r.reff.trim());
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  };

  /** Kill the outstanding link. The daemon refuses future redemptions of it —
   *  this is the "that link left the building" control. */
  const revoke = async () => {
    if (!ticket) return;
    if (!await ask.confirm({
      title: "Revoke this invite link?",
      body: "Anyone who has not joined yet will be unable to use it. Existing members keep their access. You can create a new link afterward.",
      confirmText: "Revoke invite",
      danger: true,
    })) return;
    try {
      await rpc(spaceId, { cmd: "invite_revoke", invite: ticket });
      setTicket(null);
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <section>
      <h2 className="text-base font-semibold">Invite</h2>
      <div className="border-line flex flex-col gap-3 rounded-surface border p-3">
        {!link ? (
          <>
            <div className="flex flex-wrap items-center gap-2">
              <Combobox
                label="Role"
                value={{
                  id: role,
                  label: INVITE_ROLES.find((r) => r.id === role)?.label ?? role,
                }}
                options={INVITE_ROLES.map((r) => ({ id: r.id, label: r.label, hint: r.hint }))}
                onPick={setRole}
              />
              <Combobox
                label="Expires"
                value={{
                  id: String(ttl),
                  label: INVITE_TTLS.find((t) => t.hours === ttl)?.label ?? `${ttl}h`,
                }}
                options={INVITE_TTLS.map((t) => ({ id: String(t.hours), label: t.label }))}
                onPick={(id) => setTtl(Number(id))}
              />
              <label className="text-dim flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={reusable}
                  onChange={(e) => setReusable(e.target.checked)}
                />
                Reusable — admits anyone with the link until it expires
              </label>
            </div>
            <p className="text-mute text-xs">Invite links are access capabilities. Share them only with intended recipients and revoke exposed links promptly.</p>
            <Button
              onClick={() => void mint()}
              className="w-fit"
              icon={<UserPlus className="size-icon-sm" />}
              label="Create invite link"
              variant="secondary"
              size="md"
            />
          </>
        ) : (
          <>
            <div className="flex gap-4">
              {qr && (
                <img
                  src={qr}
                  alt="Invite link QR code"
                  className="size-[110px] shrink-0 rounded-surface bg-white p-1"
                />
              )}
              <div className="flex min-w-0 flex-1 flex-col gap-2">
                <p className="text-dim text-sm">
                  They open lait and paste this into{" "}
                  <span className="text-fg font-medium">Use an invite</span> — accepting the invite
                  is the approval.
                </p>
                <code className="bg-bg border-line block truncate rounded-surface border p-2 text-xs">
                  {link}
                </code>
                <div className="flex gap-2">
                  <Button
                    variant="secondary"
                    size="sm"
                    onClick={() => {
                      void navigator.clipboard.writeText(link).then(() => {
                        setCopied(true);
                        window.setTimeout(() => setCopied(false), 1500);
                      });
                    }}
                    icon={
                      copied ? (
                        <Check className="size-icon-sm" />
                      ) : (
                        <Copy className="size-icon-sm" />
                      )
                    }
                    label={copied ? "Copied" : "Copy link"}
                  />
                  <a
                    href={mailto(link)}
                    className="control-hover-outline border-line-strong hover:bg-hover flex items-center gap-1.5 rounded-control border px-2 py-1 text-sm"
                  >
                    <Link2 className="size-icon-sm" />
                    Email it
                  </a>
                  <Button
                    onClick={() => void revoke()}
                    tooltip="The daemon refuses any future redemption of this link"
                    icon={<ShieldAlert className="size-icon-sm" />}
                    label="Revoke"
                    variant="danger"
                    size="sm"
                  />
                  <Button
                    onClick={() => setTicket(null)}
                    className="ml-auto"
                    icon={<KeyRound className="size-icon-sm" />}
                    label="New link"
                    variant="ghost"
                    size="sm"
                  />
                </div>
              </div>
            </div>
          </>
        )}
      </div>
    </section>
  );
}

/** A prefilled mail draft — no SMTP, no app password: it opens their mail client. */
function mailto(link: string): string {
  const body = [
    "You've been invited to a lait space.",
    "",
    "1. Install lait:  https://github.com/Nixie-Tech-LLC/lait",
    "2. Run lait to open the app, choose \"Use an invite\", and paste this link:",
    `   ${link}`,
    "",
    "That creates your encrypted store on your own machine and reaches me to",
    "approve you — the board stays sealed until that lands.",
  ].join("\n");
  return `mailto:?subject=${encodeURIComponent("Join my lait space")}&body=${encodeURIComponent(body)}`;
}
