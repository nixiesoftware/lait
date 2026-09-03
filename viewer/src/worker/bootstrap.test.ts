import { describe, expect, it } from "vitest";

import { FOUNDATION_RELAY, parseJoin } from "./bootstrap";

/** The topology choice reads the URL fragment, never a dev flag: a join link
 *  (a ticket, no seed) picks the in-tab engine; anything else keeps the
 *  head topology. The relay is optional — a shared foundation link carries only
 *  the ticket and defaults to the foundation relay. */
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

  it("defaults the relay to the foundation relay when none is given — a shared foundation.pub link", () => {
    expect(parseJoin("#join=t")).toEqual({ ticket: "t", relay: FOUNDATION_RELAY });
  });

  it("never reads a seed from the URL", () => {
    // Even if one were smuggled in, parseJoin exposes only ticket + relay.
    const parsed = parseJoin("#join=t&relay=r&seed=deadbeef");
    expect(parsed).toEqual({ ticket: "t", relay: "r" });
    expect(Object.keys(parsed ?? {})).toStrictEqual(["ticket", "relay"]);
  });
});
