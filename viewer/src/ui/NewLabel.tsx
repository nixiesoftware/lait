import { useState } from "react";

import { ColorPicker } from "./ColorPicker";
import { Button, Dialog, DialogHeader } from "@astryxdesign/core";

/**
 * The colour step for a label the picker is about to mint.
 *
 * A label typed into the picker used to be born gray with no say in the matter —
 * the engine has always taken a colour (`label_new {name, color}`), the viewer just
 * never asked. This is the ask: a small modal, because the picker that summoned it
 * has already closed, so there is nothing left to anchor a popover to. It only
 * chooses the colour; the caller owns the two requests that follow (register the
 * label, then attach it), because only the caller knows the issue it attaches to.
 */
export function NewLabelDialog({
  name,
  onCancel,
  onCreate,
}: {
  name: string;
  onCancel: () => void;
  onCreate: (name: string, color: string) => void;
}) {
  const [color, setColor] = useState("blue");
  return (
    <Dialog isOpen onOpenChange={(o) => !o && onCancel()} width={360} purpose="form">
      <DialogHeader
        title="New label"
        subtitle={`Pick a colour for ${name}.`}
        onOpenChange={(o) => !o && onCancel()}
      />
      <div className="p-4">
        <ColorPicker value={color} onChange={setColor} />
        <div className="mt-4 flex justify-end gap-2">
          <Button
            onClick={onCancel}
            label="Cancel"
            variant="secondary"
            elevation="low"
            size="md"
          />
          <Button
            onClick={() => onCreate(name, color)}
            label="Create label"
            variant="primary"
            size="md"
          />
        </div>
      </div>
    </Dialog>
  );
}
