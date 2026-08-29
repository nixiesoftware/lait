#!/usr/bin/env python3
"""Regenerate `viewer/src/ui/emptyArt.tsx`.

    python3 viewer/tools/empty-art/generate.py            # rewrite the component
    python3 viewer/tools/empty-art/generate.py --preview  # ...and a contact sheet

Needs Pillow, and nothing else. The component it writes is checked in, so this
script is not part of any build: running it and finding no diff is the check
that the source of the plates still matches what ships.

Everything about the drawings lives in `solids.py` (what the objects are) and
`screen.py` (how the light and the ruling turn them into tone). Edit those; this
file only packs the result and stamps it into the TSX.
"""
import argparse
import base64
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import screen
import solids

HERE = os.path.dirname(os.path.abspath(__file__))
TARGET = os.path.normpath(os.path.join(HERE, "..", "..", "src", "ui", "emptyArt.tsx"))

# What each plate is a picture of. These land in the generated file as comments,
# because a wall of base64 with no note beside it is unreviewable.
SUBJECTS = {
    "activity": "a paper roll, part of it unspooled",
    "archive": "a tall lidded crate",
    "filtered": "a funnel, lit inside, with nothing under it",
    "inbox": "a hopper, seen into",
    "issues": "a block of cards, its layers cut into the cheeks",
    "people": "two pieces on a board",
    "projects": "three cubes, set apart",
    "space": "a house — somewhere the work lives",
    "specs": "a ream with a pen across it",
    "unavailable": "one link, parted",
}

MARKER = "const FIELDS: Record<EmptyStateArt, string> = {"
END = "};\n\nconst decoded"


def pack(name):
    """One nibble per cell, row-major, base64."""
    field, _, _ = screen.tone_field(name)
    values = [max(0, min(15, int(round(field[j][i] * 15))))
              for j in range(screen.ROWS) for i in range(screen.COLS)]
    packed = bytearray()
    for k in range(0, len(values), 2):
        packed.append((values[k] << 4) | values[k + 1])
    return base64.b64encode(bytes(packed)).decode()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--preview", action="store_true", help="also write preview.png beside this script")
    args = ap.parse_args()

    names = sorted(solids.ART)
    assert set(names) == set(SUBJECTS), "SUBJECTS and solids.ART disagree"

    source = open(TARGET).read()
    head = source[:source.index(MARKER)]
    tail = source[source.index(END):]

    lines = [MARKER]
    for name in names:
        encoded = pack(name)
        lines.append(f"  // {SUBJECTS[name]}")
        lines.append(f"  {name}:")
        chunks = [encoded[i:i + 100] for i in range(0, len(encoded), 100)]
        for i, chunk in enumerate(chunks):
            lines.append(f'    "{chunk}"' + ("," if i == len(chunks) - 1 else " +"))
    open(TARGET, "w").write(head + "\n".join(lines) + "\n" + tail)
    print(f"wrote {len(names)} fields to {TARGET}")

    if args.preview:
        from PIL import Image, ImageDraw
        w, h, pad = int(screen.BOXW), int(screen.BOXH), 8
        sheet = Image.new("RGB", ((w + pad) * 5 + pad, (h + pad + 16) * 2 + pad), (16, 16, 18))
        label = ImageDraw.Draw(sheet)
        for i, name in enumerate(names):
            field, _, _ = screen.tone_field(name)
            x, y = pad + i % 5 * (w + pad), pad + i // 5 * (h + pad + 16)
            sheet.paste(screen.draw(field), (x, y))
            label.text((x + 2, y + h + 2), name, fill=(140, 140, 148))
        out = os.path.join(HERE, "preview.png")
        sheet.save(out)
        print(f"wrote {out}")


if __name__ == "__main__":
    main()
