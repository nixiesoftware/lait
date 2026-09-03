import { describe, expect, it } from "vitest";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

import { AgentProfilePage, filterCards, listedCards, partCards } from "./book";
import { fixtureClientView, type Agent, type Card } from "./client";
import { pictureUri, presenceLabel } from "./kit";

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

  it("never lists the claimed card: the canonical band is its one presentation", () => {
    const rows = [
      card({ card: "crd_me", selfClaim: true, presence: "online" }),
      card({ card: "crd_ada", presence: "online" }),
    ];
    expect(listedCards(rows).map((row) => row.card)).toEqual(["crd_ada"]);
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

  it("offers agent management only from explicit canManage authority", () => {
    const agent: Agent = {
      profile: "prf_adam", owner: "prf_owner", name: "Adam",
      introduction: "Adam is a virtual assistant.", lifecycle: "active",
      canManage: false, recordRevision: 2, inventoryRevision: 4,
      inventoryVisibility: "public", primitives: [],
    };
    const draw = (current: Agent) => renderToStaticMarkup(createElement(AgentProfilePage, {
      agent: current,
      view: { ...fixtureClientView, agent: current },
      dispatch: async () => undefined,
      onBack: () => undefined,
    }));
    expect(draw(agent)).not.toContain("Manage Adam");
    expect(draw(agent)).not.toContain("Inventory visibility");
    expect(draw({ ...agent, canManage: true })).toContain("Manage Adam");
    expect(draw({ ...agent, canManage: true })).toContain("Inventory visibility");
  });

  it("does not present authored primitive standing as live provider health", () => {
    const agent: Agent = {
      profile: "prf_adam", owner: "prf_owner", name: "Adam",
      introduction: "Adam is a virtual assistant.", lifecycle: "active",
      canManage: true, recordRevision: 2, inventoryRevision: 4,
      inventoryVisibility: "public",
      primitives: [{
        id: "console", primitive: "lait.console", label: "Console",
        summary: "Owner-authorized work.", standing: "ready",
        operationalStanding: "unavailable", visibility: "public", editable: false,
      }],
    };
    const markup = renderToStaticMarkup(createElement(AgentProfilePage, {
      agent,
      view: { ...fixtureClientView, agent },
      dispatch: async () => undefined,
      onBack: () => undefined,
    }));
    expect(markup).toContain("live: unavailable");
    expect(markup).toContain("Configured: ready");
    expect(markup).not.toContain("live: ready");
  });
});
