import { useState } from "react";
import { X } from "lucide-react";

import { rpc } from "../api";
import { ColorPicker } from "./ColorPicker";
import { Button, Dialog, IconButton, TextInput } from "@astryxdesign/core";


/**
 * The project composer.
 *
 * Two fields, and the second one is the whole reason this is a dialog rather than a
 * one-line prompt: the **key** is not metadata, it is the name every issue in this
 * project will be called forever. `ENG-142` is what goes in a branch name, a commit
 * message, and a teammate's chat. Picking it deserves to be shown, not typed blind
 * into a box labelled "key".
 *
 * So the key derives from the name as you type — the overwhelmingly common case is
 * the first few letters — and stops the moment you touch it, because a derived value
 * that keeps overwriting your edit is worse than no derivation at all.
 *
 * The rules are mirrored from `replica.rs::project_new` for *feedback*, never for
 * enforcement: the daemon validates and its refusal is the answer. What this buys is
 * that you find out before you press the button rather than after.
 */

/** 1–8 ASCII letters. Anything else breaks `KEY-n` parsing and branch inference. */
const KEY_RE = /^[A-Za-z]{1,8}$/;

export function NewProject({
  spaceId,
  taken,
  onClose,
  onCreated,
}: {
  spaceId: string;
  /** Existing keys, uppercased — the daemon refuses a duplicate. */
  taken: string[];
  onClose: () => void;
  onCreated: (key: string) => void;
}) {
  const [name, setName] = useState("");
  const [key, setKey] = useState("");
  const [color, setColor] = useState("blue");
  /** Once you edit the key yourself, the name stops driving it. */
  const [manual, setManual] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState("");

  const derived = manual ? key : deriveKey(name);
  const upper = derived.toUpperCase();

  const problem = projectKeyProblem(derived, taken);

  const ready = name.trim() !== "" && derived !== "" && !problem && !busy;

  const create = async () => {
    if (!ready) return;
    setBusy(true);
    setFailure("");
    try {
      const r = await rpc(spaceId, { cmd: "project_new", name: name.trim(), key: upper, color });
      // `project_new` replies with the key as the ref — switch the board to it, so
      // creating a project lands you in it rather than leaving you where you were.
      if (r.kind === "ref") onCreated(r.reff);
      onClose();
    } catch (e) {
      setFailure(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      isOpen
      onOpenChange={(o) => !o && onClose()}
      width={440}
      purpose="form"
      aria-labelledby="new-project-heading"
    >
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void create();
        }}
      >
        <header className="border-line flex items-center gap-2 border-b p-4">
          <h2 id="new-project-heading" className="font-semibold">New project</h2>
          {/* `type="button"` as well as `onClick`: this sits inside the form,
              and a button in a form with no type is a submit button. Without
              both, Close either did nothing (it had no handler at all) or
              created the project. */}
          <IconButton
              type="button"
              label="Close"
              className="ml-auto"
              onClick={onClose}
              variant="ghost"
              size="sm"
              tooltip="Close  Esc"
              icon={<X className="size-icon-md" />}
            />
        </header>

        <div className="flex flex-col gap-3 p-4">
          <TextInput
            label="Name"
            hasAutoFocus
            value={name}
            placeholder="Engineering"
            onChange={setName}
            onKeyDown={(e) => e.stopPropagation()}
            width="100%"
          />

          <TextInput
            label="Key"
            value={derived}
            placeholder="ENG"
            onChange={(value) => {
              setManual(true);
              setKey(value);
            }}
            onKeyDown={(e) => e.stopPropagation()}
            className="font-mono uppercase"
            width="100%"
            description={
              upper
                ? `Issues here will be ${upper}-1, ${upper}-2…`
                : "Becomes the KEY in KEY-1 — 1–8 letters"
            }
            {...(problem !== null ? { status: { type: "error" as const, message: problem } } : {})}
          />

          <div className="flex flex-col gap-1.5">
            <span className="text-mute text-2xs uppercase">Color</span>
            <ColorPicker value={color} onChange={setColor} />
          </div>
          {failure && (
            <p className="border-danger/25 bg-danger/5 text-danger rounded-surface border p-2 text-xs" role="alert">
              Project not created. Your name and key are still here: {failure}
            </p>
          )}
        </div>

        {/* `px-4` to line the commit up with the fields above it, and no ⏎
            badge: it was the app's only `<kbd>` outside a menu row, and the
            form already submits on Enter. The shortcut lives on the tooltip,
            where a hint belongs. */}
        <footer className="border-line flex items-center justify-end gap-2 border-t px-4 py-3">
          <Button
            type="submit"
            isDisabled={!ready}
            isLoading={busy}
            label={busy ? "Creating…" : "Create project"}
            tooltip="Create project  ⏎"
            variant="primary"
            size="md"
          />
        </footer>
      </form>
    </Dialog>
  );
}

/**
 * A first guess at the key from the name.
 *
 * Initials for a multi-word name (`Design System` → `DS`), otherwise the first three
 * letters (`Engineering` → `ENG`). Non-letters are dropped rather than rejected,
 * because a name like "Web 2.0" should still suggest something rather than nothing.
 */
export function deriveKey(name: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean);
  if (words.length === 0) return "";
  if (words.length > 1) {
    return words
      .map((w) => w.replace(/[^A-Za-z]/g, "")[0] ?? "")
      .join("")
      .slice(0, 8)
      .toUpperCase();
  }
  return (words[0] ?? "").replace(/[^A-Za-z]/g, "").slice(0, 3).toUpperCase();
}

export function projectKeyProblem(key: string, taken: readonly string[]): string | null {
  if (!key) return null;
  const upper = key.toUpperCase();
  if (!KEY_RE.test(key)) return "1–8 letters, no digits or punctuation";
  if (taken.includes(upper)) return `${upper} is already a project here`;
  return null;
}
