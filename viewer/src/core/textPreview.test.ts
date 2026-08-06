import { describe, expect, it } from "vitest";

import { continueTextPreview, textRevision, textSplice } from "./textPreview";

describe("cumulative text previews", () => {
  it("keeps the receiver-known base after an intermediate acknowledgement", () => {
    const base = "paragraph";
    const firstText = `${base}f`;
    const first = {
      ...textSplice(base, firstText)!,
      base: textRevision(base),
      result: textRevision(firstText),
    };

    // The first character is already durable locally, but a remote viewer may
    // still hold `base`. The second preview must therefore remain cumulative
    // from `base`, rather than move its base to `firstText` and disappear there.
    const secondText = `${base}fa`;
    expect(continueTextPreview(
      first,
      textRevision(firstText),
      textRevision(firstText),
      firstText,
      secondText,
      textSplice(firstText, secondText)!,
    )).toEqual({
      base: textRevision(base),
      index: base.length,
      delete: 0,
      insert: "fa",
    });
  });

  it("rebases when the next edit falls outside the cumulative insertion", () => {
    const previous = {
      index: 4,
      delete: 0,
      insert: "fast",
      base: textRevision("slow"),
      result: textRevision("slowfast"),
    };
    const settled = "slowfast";
    const current = "Slowfast";

    expect(continueTextPreview(
      previous,
      textRevision(settled),
      textRevision(settled),
      settled,
      current,
      { index: 0, delete: 1, insert: "S" },
    )).toEqual({
      base: textRevision(settled),
      index: 0,
      delete: 1,
      insert: "S",
    });
  });
});
