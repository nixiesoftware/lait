import { useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";

import { SPEC_KIND_BLURB, SPEC_KIND_LABEL, SPEC_KINDS } from "../core/specs";
import type { SpecKind } from "../types";
import { Button, Input, cn } from "./primitives";

/**
 * The two questions a new Spec has to answer.
 *
 * Kind and title, and nothing else — a Spec is born as a draft with an empty
 * body, and every other fact it will ever carry (its revisions, what it governs,
 * whether it has been issued) is something that *happens* to it later. Asking
 * for any of that here would be asking the writer to predict the document.
 *
 * Kind leads because it is the one choice typing cannot undo: the body is a
 * revision away, the title is a revision away, and the kind is what decides
 * whether this thing will ever govern anything. It is offered as the full list
 * rather than a select — ten options, each needing a sentence to choose between,
 * is a list to read, not a value to pick.
 */
export function NewSpecDialog({
  projectName,
  kind: seed,
  onCancel,
  onCreate,
}: {
  projectName: string;
  /** Opened from a kind's own group header, which has already answered the
   *  first question. Absent means the dialog asks it. */
  kind?: SpecKind;
  onCancel: () => void;
  onCreate: (kind: SpecKind, title: string) => void;
}) {
  const [kind, setKind] = useState<SpecKind>(seed ?? "requirement");
  const [title, setTitle] = useState("");
  const empty = title.trim() === "";

  return (
    <Dialog.Root open onOpenChange={(open) => !open && onCancel()}>
      <Dialog.Portal>
        <Dialog.Overlay className="ui-overlay fixed inset-0 z-50 bg-black/45 backdrop-blur-[2px]" />
        <Dialog.Content
          className="ui-surface border-line-strong bg-raised shadow-overlay fixed top-[12vh] left-1/2 z-50 flex max-h-[76vh] w-[min(520px,92vw)] -translate-x-1/2 flex-col rounded-surface border"
          aria-describedby={undefined}
        >
          <form
            className="flex min-h-0 flex-col"
            onSubmit={(event) => {
              event.preventDefault();
              if (!empty) onCreate(kind, title.trim());
            }}
          >
            <div className="border-line border-b p-4">
              <Dialog.Title className="font-semibold">New spec in {projectName}</Dialog.Title>
            </div>
            <div className="min-h-0 flex-1 overflow-y-auto p-2">
              {SPEC_KINDS.map((option) => (
                <button
                  key={option}
                  type="button"
                  onClick={() => setKind(option)}
                  aria-pressed={option === kind}
                  className={cn(
                    "flex w-full flex-col gap-0.5 rounded-control px-3 py-2 text-left transition-colors",
                    option === kind ? "bg-active text-fg" : "hover:bg-hover",
                  )}
                >
                  <span className="text-sm font-medium">{SPEC_KIND_LABEL[option]}</span>
                  <span className="text-mute text-xs">{SPEC_KIND_BLURB[option]}</span>
                </button>
              ))}
            </div>
            <div className="border-line border-t p-4">
              <Input
                autoFocus
                value={title}
                placeholder={`${SPEC_KIND_LABEL[kind]} title`}
                onChange={(event) => setTitle(event.target.value)}
                // Radix closes on Escape; stopping propagation keeps the app's
                // global keymap from also acting on the same keystroke.
                onKeyDown={(event) => event.stopPropagation()}
                className="w-full"
                aria-label="Title"
              />
            </div>
            <div className="border-line flex justify-end gap-2 border-t p-3">
              <Dialog.Close asChild>
                <Button size="md" variant="outline" type="button">
                  Cancel
                </Button>
              </Dialog.Close>
              <Button size="md" variant="primary" type="submit" disabled={empty}>
                Create spec
              </Button>
            </div>
          </form>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
