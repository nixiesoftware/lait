import { describe, expect, it } from "vitest";

import { filterCards, partCards } from "./book";
import type { Card } from "./client";
import { isAgentCard, pictureUri, presenceLabel } from "./kit";

function card(overrides: Partial<Card> & { card: string }): Card {
  return {
    name: overrides.card,
    note: "",
    handles: [],
    addresses: [],
    devices: [],
    agents: [],
    picture: null,
    groups: [],
    selfClaim: false,
    presence: null,
    ...overrides,
  };
}

describe("the book's list rules", () => {
  it("parts by presence alone: present and unmeasured above, measured absence below", () => {
    const rows = [
      card({ card: "offline", presence: "offline" }),
      card({ card: "unmeasured", presence: null }),
      card({ card: "away", presence: "away" }),
      card({ card: "online", presence: "online" }),
    ];
    const { contacts, offline } = partCards(rows);
    // Ordered by how present they are; "could not be asked" is not a lesser
    // Offline, so the unmeasured card stays up top with the contacts.
    expect(contacts.map((row) => row.card)).toEqual(["online", "away", "unmeasured"]);
    expect(offline.map((row) => row.card)).toEqual(["offline"]);
  });

  it("searches what a card says about itself: name, note, id, handles", () => {
    const rows = [
      card({ card: "crd_a", name: "Ada", note: "met at the workshop" }),
      card({ card: "crd_b", name: "Brin", handles: ["actor:space:brin"] }),
    ];
    expect(filterCards(rows, "").map((row) => row.card)).toEqual(["crd_a", "crd_b"]);
    expect(filterCards(rows, "workshop").map((row) => row.card)).toEqual(["crd_a"]);
    expect(filterCards(rows, "ACTOR:space").map((row) => row.card)).toEqual(["crd_b"]);
    expect(filterCards(rows, "crd_a").map((row) => row.card)).toEqual(["crd_a"]);
    expect(filterCards(rows, "nobody")).toEqual([]);
  });

  it("marks an agent by group or by agents-only handles, never by mixed cards", () => {
    expect(isAgentCard(card({ card: "a", groups: ["Agents"] }))).toBe(true);
    expect(isAgentCard(card({ card: "b", agents: ["agent:h:name"] }))).toBe(true);
    // A person's card may list co-located agents; an address anchors it.
    expect(isAgentCard(card({ card: "c", agents: ["agent:h:name"], addresses: ["actor:s:c"] }))).toBe(false);
    expect(isAgentCard(card({ card: "d" }))).toBe(false);
  });

  it("words a measured presence and says nothing for an absence", () => {
    expect(presenceLabel("online")).toBe("Online");
    expect(presenceLabel("offline")).toBe("Offline");
    expect(presenceLabel(null)).toBeNull();
  });

  it("resolves the stored picture form and refuses anything else", () => {
    expect(pictureUri("image/png;base64,QUJD")).toBe("data:image/png;base64,QUJD");
    expect(pictureUri("not-a-picture")).toBeNull();
    expect(pictureUri(null)).toBeNull();
  });
});
