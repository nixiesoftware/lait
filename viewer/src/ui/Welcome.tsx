import { useEffect, useState } from "react";
import { FolderPlus, Link2, Loader2 } from "lucide-react";

import { hostRpc } from "../api";
import type { HostReply } from "../types";
import { Button, FieldLabel, Input } from "./primitives";
import { InlineError } from "./AppState";

/**
 * Founding a Space, and entering one from an invite.
 *
 * This is the *only* client of `POST /api/host/rpc`, and the reason that route
 * exists: every other endpoint is `/api/spaces/{id}/…`, which is unreachable at
 * the one moment these two matter — there is no space id yet. Without this
 * panel a machine with no store opens a page that can do nothing at all, and
 * the refusal `host_client::no_store_here` prints ("run `lait` to open the local
 * app, then found a space or join one from an invite") names a remedy nobody
 * can carry out.
 *
 * Everything it sends carries an explicit store directory, because the daemon's
 * working directory is not the person's and nothing is created implicitly. The
 * node proposes one (`host_context.spaces_root`) so the path box is never empty.
 */
export function Welcome({
  onArrived,
  initialMode = "found",
  onCancel,
}: {
  onArrived: (space: string) => void;
  /** Which tab to open on. A caller that already knows the answer — an empty
   *  state naming a space this device does not hold — should not make someone
   *  pick it again; the generic "Add space" entry has no such knowledge and
   *  takes the default. */
  initialMode?: "found" | "enter";
  /** Provided only when there is somewhere to go back to. On a machine with no
   *  store there is not, and a cancel button that strands you is worse than
   *  none. */
  onCancel?: (() => void) | undefined;
}) {
  const [mode, setMode] = useState<"found" | "enter">(initialMode);
  const [context, setContext] = useState<Extract<HostReply, { host: "context" }> | null>(null);
  const [name, setName] = useState("");
  const [link, setLink] = useState("");
  const [nick, setNick] = useState("");
  const [home, setHome] = useState("");
  // True once the person edits the path themselves: after that, typing a name
  // must not silently move the store they chose.
  const [pinnedHome, setPinnedHome] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [waiting, setWaiting] = useState("");

  useEffect(() => {
    let live = true;
    void hostRpc({ cmd: "host_context" })
      .then((reply) => {
        if (live && reply.kind === "host" && reply.host === "context") setContext(reply);
      })
      .catch((e: unknown) => {
        if (live) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      live = false;
    };
  }, []);

  const suggested = context ? join(context.spaces_root, slug(mode === "found" ? name : link)) : "";
  const target = pinnedHome ? home : suggested;

  const submit = async () => {
    setError("");
    setBusy(true);
    // Entering drives admission before it answers — up to half a minute against
    // an inviter who may be offline — so say what the wait is for rather than
    // leaving a dead button.
    setWaiting(mode === "enter" ? "Reaching the inviter…" : "");
    try {
      const reply = await hostRpc(
        mode === "found"
          ? { cmd: "host_space_found", home: target, name: name.trim(), nick: nick.trim() || null }
          : { cmd: "host_space_enter", link: link.trim(), home: target, nick: nick.trim() || null },
      );
      if (reply.kind !== "host") throw new Error("unexpected reply");
      if (reply.host === "founded") {
        onArrived(reply.space);
        return;
      }
      if (reply.host === "entered") {
        // Bootstrapped is not admitted. Every Body in the store is still sealed
        // to a key admission delivers, so opening it now shows an empty board
        // and calls it a space — say so instead.
        if (!reply.admitted) {
          setError(
            reply.contacted
              ? `Joined ${reply.space}, but standing has not landed yet — the board stays encrypted until the inviter approves. ${reply.last_error ?? ""}`
              : `Joined ${reply.space}, but the inviter did not answer. The store is bound; open it again when they are online.`,
          );
        }
        onArrived(reply.space);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
      setWaiting("");
    }
  };

  const ready = target.trim() !== "" && (mode === "found" ? name.trim() !== "" : link.trim() !== "");

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto flex max-w-lg flex-col gap-5 p-8">
        <header>
          <h1 className="text-lg font-semibold">
            {mode === "found" ? "Start a space" : "Enter a space"}
          </h1>
          <p className="text-dim mt-1 text-sm">
            A space is an encrypted store on this machine. Found one, or enter one you were invited
            to — nothing is created until you say so.
          </p>
        </header>

        <div className="border-line flex gap-1 rounded-surface border p-1">
          {(
            [
              { id: "found", label: "Found a space", icon: <FolderPlus className="size-icon-sm" /> },
              { id: "enter", label: "Use an invite", icon: <Link2 className="size-icon-sm" /> },
            ] as const
          ).map((t) => (
            <button
              key={t.id}
              onClick={() => setMode(t.id)}
              className={`flex flex-1 items-center justify-center gap-2 rounded-control px-3 py-1.5 text-sm ${
                mode === t.id ? "bg-raised font-medium" : "text-dim"
              }`}
            >
              {t.icon}
              {t.label}
            </button>
          ))}
        </div>

        {mode === "found" ? (
          <FieldLabel>
            <span>Name</span>
            <Input
              autoFocus
              value={name}
              placeholder="Engineering"
              onChange={(e) => setName(e.target.value)}
            />
          </FieldLabel>
        ) : (
          <FieldLabel>
            <span>Invite link</span>
            <Input
              autoFocus
              value={link}
              placeholder="lait://…"
              onChange={(e) => setLink(e.target.value)}
            />
          </FieldLabel>
        )}

        <FieldLabel>
          <span>Store directory</span>
          <span className="text-mute text-xs">
            On the machine running lait. It holds the encrypted replica.
          </span>
          <Input
            value={target}
            placeholder={context ? context.spaces_root : "Loading…"}
            onChange={(e) => {
              setPinnedHome(true);
              setHome(e.target.value);
            }}
          />
        </FieldLabel>

        <FieldLabel>
          <span>Your name in this space</span>
          <Input value={nick} placeholder="ada (optional)" onChange={(e) => setNick(e.target.value)} />
        </FieldLabel>

        {error && <InlineError message={error} onDismiss={() => setError("")} />}

        <div className="flex items-center gap-3">
          <Button variant="primary" disabled={!ready || busy} loading={busy} onClick={() => void submit()}>
            {mode === "found" ? "Found it" : "Enter"}
          </Button>
          {onCancel && (
            <Button disabled={busy} onClick={onCancel}>
              Cancel
            </Button>
          )}
          {waiting && busy && (
            <span className="text-dim flex items-center gap-1.5 text-sm">
              <Loader2 className="size-icon-xs animate-spin" />
              {waiting}
            </span>
          )}
        </div>

        {context && (
          <p className="text-mute text-xs">
            lait {context.version} · identity at {context.identity_home}
          </p>
        )}
      </div>
    </div>
  );
}

/** A directory name from free text: what a person typed, minus what a path
 *  separator would turn into a second directory. */
function slug(text: string): string {
  const cleaned = text
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return cleaned.slice(0, 40) || "space";
}

/** Join with the separator the *daemon's* OS uses, which is the only one that
 *  matters — the path is resolved there, not here, and the browser may be on a
 *  different machine's convention entirely. */
function join(root: string, leaf: string): string {
  // One backslash anywhere is enough to say which OS wrote this: a Windows
  // config root reached through a forward-slash `$LAIT_CONFIG_ROOT` comes back
  // mixed, and appending `/` to it would hand the person a path spelled two
  // ways in one line.
  const sep = root.includes("\\") ? "\\" : "/";
  return `${root.replace(/[\\/]+$/, "")}${sep}${leaf}`;
}
