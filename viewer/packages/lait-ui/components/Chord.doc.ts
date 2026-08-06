import type { ComponentDoc } from "@astryxdesign/cli/authoring";

const doc: ComponentDoc = {
  type: "component",
  name: "Chord",
  description:
    "A keyboard chord, already formatted by lait's binding registry. Use where a control's shortcut is shown beside it — menu rows, tooltips, the palette.",
  props: [
    {
      name: "children",
      type: "ReactNode",
      description:
        "The chord as it should read, e.g. the output of formatBinding(k, { glyphs: true }). Not a key spec — this component does no formatting of its own.",
    },
    {
      name: "className",
      type: "string",
      description: "Extra classes. Layout only; the chip's own look is fixed.",
    },
  ],
  usage: [
    "Prefer this over Astryx's Kbd inside lait: Astryx's takes a `keys` grammar and formats it itself, which would put a second formatter over one binding.",
    "The registry decides what a shortcut is. This only draws it.",
  ],
};

export default doc;
