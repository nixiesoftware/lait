import { useEffect, useState } from "react";
import { ArrowLeft, FolderPlus, Link2, Loader2 } from "lucide-react";

import { hostRpc } from "../api";
import type { HostReply } from "../types";
import { Button, TextInput } from "@astryxdesign/core";

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
    /**
     * The whole window, not a panel inside it.
     *
     * This used to render in the work area with the shell still around it — a
     * sidebar offering Inbox, My issues, Projects and Roadmap, and a tree
     * headed "PROJECTS 0 · No projects yet". Every one of those is a
     * destination that cannot exist yet: the surface whose entire job is "there
     * is no space here" was framed by a navigation tree for a space. It read as
     * a page you had arrived at rather than the one thing there was to do.
     *
     * So it takes the viewport. There is no chrome because there is nothing yet
     * to navigate, and the two things that are not this form — the way back,
     * and which build you are running — sit in the corners where they cannot be
     * mistaken for part of it.
     */
    <div className="bg-bg fixed inset-0 z-50 flex flex-col overflow-y-auto">
      {/* Corner affordances, on their own row so the form below stays optically
          centred rather than being pushed down by them. */}
      <div className="flex shrink-0 items-start justify-between p-4">
        {onCancel ? (
          <Button
            isDisabled={busy}
            onClick={onCancel}
            label="Back"
            icon={<ArrowLeft className="size-icon-sm" />}
            variant="ghost"
            size="sm"
          />
        ) : (
          <span />
        )}
        {context && (
          <span className="text-mute text-xs">
            lait {context.version}
          </span>
        )}
      </div>

      <div className="flex min-h-0 flex-1 items-center justify-center px-6 pb-16">
      <div className="flex w-full max-w-[26rem] flex-col gap-5">
        {/* Centred, because there is nothing to the left of it to align to. The
            fields below stay left-aligned: a label is read down a column. */}
        <header className="text-center">
          <h1 className="text-xl font-semibold">
            {mode === "found" ? "Start a space" : "Enter a space"}
          </h1>
          <p className="text-dim mt-1.5 text-sm">
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

        {/* The fields are a card and the commit is not.
            One is the thing you fill in, the other is the thing you do with it,
            and drawing the boundary between them is what stops a form of four
            stacked controls reading as five. `width="100%"` on each: Astryx's
            inputs size to their content, so without it they collapse to a
            ~150px stub inside a 416px column. */}
        <div className="border-line bg-raised flex flex-col gap-4 rounded-surface border p-4">
          {mode === "found" ? (
            <TextInput
              label="Name"
              hasAutoFocus
              value={name}
              placeholder="Engineering"
              onChange={setName}
              width="100%"
            />
          ) : (
            <TextInput
              label="Invite link"
              hasAutoFocus
              value={link}
              placeholder="lait://…"
              onChange={setLink}
              width="100%"
            />
          )}

          {/* The helper line is `description`, not a second span: Astryx's field
              owns label, description and status, so it places and associates them
              for the screen reader instead of us stacking three siblings. */}
          <TextInput
            label="Store directory"
            description="On the machine running lait. It holds the encrypted replica."
            value={target}
            placeholder={context ? context.spaces_root : "Loading…"}
            onChange={(value) => {
              setPinnedHome(true);
              setHome(value);
            }}
            width="100%"
          />

          <TextInput
            label="Your name in this space"
            value={nick}
            placeholder="ada (optional)"
            onChange={setNick}
            isOptional
            width="100%"
          />
        </div>

        {error && <InlineError message={error} onDismiss={() => setError("")} />}

        {/* Full width, under the card. There is exactly one thing to do here,
            so it does not need to be found — and a commit that spans the form
            it commits says so without a word. Cancel is not beside it: it went
            to the corner, because an escape hatch sitting next to the action is
            a thing to mis-click, not a thing to reach for. */}
        <Button
          isDisabled={!ready || busy}
          isLoading={busy}
          onClick={() => void submit()}
          label={mode === "found" ? "Found it" : "Enter"}
          variant="primary"
          size="md"
          className="w-full"
        />

        {waiting && busy && (
          <span className="text-dim flex items-center justify-center gap-1.5 text-sm">
            <Loader2 className="size-icon-xs animate-spin" />
            {waiting}
          </span>
        )}

      </div>
      </div>

      {/* Where the identity actually lives — a fact about this machine, not a
          field of this form, so it sits in the opposite corner from the build
          it belongs with. Truncated with the whole path on the title, because a
          store root can be long enough to wrap twice and it is diagnostic
          information, not something anyone reads across. */}
      {context && (
        <p
          className="text-mute shrink-0 truncate p-4 text-xs"
          title={context.identity_home}
        >
          identity at {context.identity_home}
        </p>
      )}
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
