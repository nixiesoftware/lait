import { useEffect, useState } from "react";

import { AlertDialog, Button, Dialog, DialogHeader, TextInput } from "@astryxdesign/core";

/**
 * Ask the user something, using our components.
 *
 * These replace `window.prompt` / `window.confirm`, which were never ours: they
 * ignore the theme, can't be styled, and are blocked outright wherever the page is
 * embedded or sandboxed — which is exactly where this app first got asked to run.
 * "It works in my tab" is not a design system.
 *
 * The API stays imperative and promise-based on purpose. `await ask.prompt(…)`
 * reads the same as the `window.prompt` it replaces, so the call sites stayed
 * simple and nothing had to be restructured into dialog state to gain a dialog.
 *
 * Astryx does the parts that are tedious and invisible until they're wrong: focus
 * trap, restore-focus-on-close, Escape, `aria-modal`, and scroll locking.
 * `AlertDialog` for confirmations rather than `Dialog`, because it is the
 * semantically different one — it interrupts, it demands a choice, and it does not
 * close on an outside click, which is precisely what you want between someone and
 * a destructive verb. Its `title`/`description`/`actionLabel` are required props,
 * so the a11y wiring is not something a call site can forget.
 */

interface PromptReq {
  kind: "prompt";
  title: string;
  body?: string;
  label?: string;
  placeholder?: string;
  defaultValue?: string;
  confirmText?: string;
  /** An empty answer is a legitimate one (clearing a petname), so `""` !== null. */
  allowEmpty?: boolean;
  resolve: (v: string | null) => void;
}

interface ConfirmReq {
  kind: "confirm";
  title: string;
  body?: string;
  confirmText?: string;
  danger?: boolean;
  resolve: (v: boolean) => void;
}

type Req = PromptReq | ConfirmReq;

let emit: ((r: Req) => void) | null = null;

/** Text input. Resolves `null` on cancel — cancel and "" are different answers. */
export function prompt(o: Omit<PromptReq, "kind" | "resolve">): Promise<string | null> {
  return new Promise((resolve) => {
    // No host mounted means nobody can answer; resolving null is the honest
    // outcome, and it fails closed — a write simply doesn't happen.
    if (!emit) return resolve(null);
    emit({ kind: "prompt", ...o, resolve });
  });
}

/** Yes/no. Resolves `false` if there's nobody to ask. */
export function confirm(o: Omit<ConfirmReq, "kind" | "resolve">): Promise<boolean> {
  return new Promise((resolve) => {
    if (!emit) return resolve(false);
    emit({ kind: "confirm", ...o, resolve });
  });
}

/** Mount once, near the root. */
export function DialogHost() {
  const [req, setReq] = useState<Req | null>(null);
  const [value, setValue] = useState("");

  useEffect(() => {
    emit = (r) => {
      setReq(r);
      setValue(r.kind === "prompt" ? (r.defaultValue ?? "") : "");
    };
    return () => {
      emit = null;
    };
  }, []);

  if (!req) return null;

  /** Every path out answers the promise — a dialog that resolves nothing hangs
   *  whatever awaited it, forever. */
  const settle = (v: string | null | boolean) => {
    if (req.kind === "prompt") req.resolve(v as string | null);
    else req.resolve(v as boolean);
    setReq(null);
  };

  if (req.kind === "confirm") {
    return (
      <AlertDialog
        isOpen
        onOpenChange={(open) => !open && settle(false)}
        title={req.title}
        description={req.body ?? ""}
        actionLabel={req.confirmText ?? "Confirm"}
        actionVariant={req.danger ? "destructive" : "primary"}
        onAction={() => settle(true)}
      />
    );
  }

  const empty = value.trim() === "" && !req.allowEmpty;

  return (
    <Dialog
      isOpen
      onOpenChange={(open) => !open && settle(null)}
      width={480}
      // `form`, not `info`: a half-typed answer should survive a stray click on
      // the backdrop. Escape still closes, which is the behaviour a prompt
      // replacing `window.prompt` has to keep.
      purpose="form"
    >
      <DialogHeader
        title={req.title}
        {...(req.body ? { subtitle: req.body } : {})}
        onOpenChange={(open) => !open && settle(null)}
      />
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (!empty) settle(req.allowEmpty ? value : value.trim());
        }}
      >
        <div className="p-4">
          <TextInput
            label={req.label ?? "Answer"}
            isLabelHidden={!req.label}
            hasAutoFocus
            value={value}
            {...(req.placeholder ? { placeholder: req.placeholder } : {})}
            onChange={setValue}
            // The dialog closes on Escape itself; stopping propagation keeps the
            // app's global keymap from also acting on the same keystroke.
            onKeyDown={(e) => e.stopPropagation()}
            width="100%"
          />
        </div>
        <footer className="border-line flex items-center justify-end gap-2 border-t px-4 py-3">
          <Button
            type="button"
            label="Cancel"
            variant="secondary"
            size="md"
            onClick={() => settle(null)}
          />
          <Button
            type="submit"
            isDisabled={empty}
            label={req.confirmText ?? "Save"}
            variant="primary"
            size="md"
          />
        </footer>
      </form>
    </Dialog>
  );
}
