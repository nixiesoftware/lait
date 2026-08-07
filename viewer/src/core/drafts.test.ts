import { beforeEach, describe, expect, it } from "vitest";

import { clearDraft, loadDraft, loadFields, saveDraft, saveFields } from "./drafts";

describe("private local drafts", () => {
  beforeEach(() => localStorage.clear());

  it("scopes drafts by canonical space, subject, and kind", () => {
    saveDraft("spc_a", "iss_1", "comment", "hello");
    saveDraft("spc_a", "iss_1", "title", "Recovered title");
    expect(loadDraft("spc_a", "iss_1", "comment")).toBe("hello");
    expect(loadDraft("spc_a", "iss_1", "title")).toBe("Recovered title");
    expect(loadDraft("spc_b", "iss_1", "comment")).toBe("");
    expect(loadDraft("spc_a", "iss_1", "description")).toBe("");
  });

  it("removes empty and explicitly cleared drafts", () => {
    saveDraft("spc_a", "iss_1", "comment", "hello");
    clearDraft("spc_a", "iss_1", "comment");
    expect(loadDraft("spc_a", "iss_1", "comment")).toBe("");
  });

  it("round-trips the composer's fields, not only its prose", () => {
    saveFields("spc_a", "new:EXEC", {
      status: "in-progress",
      priority: "high",
      labels: ["engine", "viewer"],
      assignees: ["me"],
      due: "2026-08-14",
      project: "EXEC",
    });
    expect(loadFields("spc_a", "new:EXEC")).toEqual({
      status: "in-progress",
      priority: "high",
      labels: ["engine", "viewer"],
      assignees: ["me"],
      due: "2026-08-14",
      project: "EXEC",
    });
    expect(loadFields("spc_b", "new:EXEC")).toEqual({});
  });

  it("leaves nothing behind when every field is at its default", () => {
    saveFields("spc_a", "new:EXEC", { priority: "high" });
    saveFields("spc_a", "new:EXEC", { labels: [], assignees: [] });
    expect(loadDraft("spc_a", "new:EXEC", "new-fields")).toBe("");
    expect(loadFields("spc_a", "new:EXEC")).toEqual({});
  });

  it("treats an unreadable blob as no draft rather than as a failure", () => {
    saveDraft("spc_a", "new:EXEC", "new-fields", "{not json");
    expect(loadFields("spc_a", "new:EXEC")).toEqual({});

    saveDraft("spc_a", "new:EXEC", "new-fields", '["engine"]');
    expect(loadFields("spc_a", "new:EXEC")).toEqual({});

    // A shape from some other build: the fields it does understand survive.
    saveDraft("spc_a", "new:EXEC", "new-fields", '{"priority":"high","estimate":5,"labels":[1,2]}');
    expect(loadFields("spc_a", "new:EXEC")).toEqual({ priority: "high" });
  });
});
