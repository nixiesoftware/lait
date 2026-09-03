import { describe, expect, it } from "vitest";

import { parseJoin } from "./bootstrap";

/** The topology choice reads the URL fragment, never a dev flag: a join link
 *  (ticket + relay, no seed) picks the in-tab engine; anything else keeps the
 *  head topology. */
describe("parseJoin", () => {
  it("reads a ticket and relay from the fragment", () => {
    expect(
      parseJoin("#join=lait://join/abc&relay=http://127.0.0.1:9000"),
    ).toEqual({ ticket: "lait://join/abc", relay: "http://127.0.0.1:9000" });
  });

  it("accepts a fragment without the leading #", () => {
    expect(parseJoin("join=t&relay=r")).toEqual({ ticket: "t", relay: "r" });
  });

  it("returns null for an ordinary load (no ticket)", () => {
    expect(parseJoin("")).toBeNull();
    expect(parseJoin("#view=board")).toBeNull();
  });

  it("returns null when the relay is missing — a ticket alone is not a join", () => {
    expect(parseJoin("#join=t")).toBeNull();
  });

  it("never reads a seed from the URL", () => {
    // Even if one were smuggled in, parseJoin exposes only ticket + relay.
    const parsed = parseJoin("#join=t&relay=r&seed=deadbeef");
    expect(parsed).toEqual({ ticket: "t", relay: "r" });
    expect(Object.keys(parsed ?? {})).toStrictEqual(["ticket", "relay"]);
  });
});
