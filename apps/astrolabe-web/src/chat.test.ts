import { describe, expect, it } from "vitest";

import { agentMessageClass, dayLabel, timeLabel, transcriptItems, type TranscriptItem } from "./chat";
import type { ChatMessage } from "./client";

const at = (iso: string): number => Math.floor(new Date(iso).getTime() / 1000);
const message = (sentAt: number, mine: boolean): ChatMessage =>
  ({ mine, kind: "message", body: "hi", invitation: null, id: null, sentAt, fromDevice: "dev", provenanceAgrees: true });

const shape = (items: TranscriptItem[]) => items.map((item) =>
  item.kind === "message" ? `msg:${item.groupStarts ? "S" : "-"}${item.groupEnds ? "E" : "-"}` : item.kind);

describe("the transcript's shape", () => {
  const now = new Date("2026-08-19T20:00:00");

  it("parts by day, then by long quiet, and groups same-sender runs", () => {
    const items = transcriptItems([
      message(at("2026-08-18T10:00:00"), false),
      message(at("2026-08-18T10:01:00"), false),
      message(at("2026-08-18T12:00:00"), false),  // > 1h quiet: a divider
      message(at("2026-08-19T09:00:00"), true),   // new day: a date pill
      message(at("2026-08-19T09:00:30"), false),  // sender flip: new group
    ], now);
    expect(shape(items)).toEqual([
      "day", "msg:S-", "msg:-E",
      "gap", "msg:SE",
      "day", "msg:SE",
      "msg:SE",
    ]);
  });

  it("names the day: Today, Yesterday, then a written date", () => {
    expect(dayLabel(new Date("2026-08-19T08:00:00"), now)).toBe("Today");
    expect(dayLabel(new Date("2026-08-18T23:00:00"), now)).toBe("Yesterday");
    expect(dayLabel(new Date("2026-08-14T12:00:00"), now)).toBe("Fri, Aug 14");
  });

  it("tells time the way a chat does", () => {
    expect(timeLabel(new Date("2026-08-19T00:05:00"))).toBe("12:05 AM");
    expect(timeLabel(new Date("2026-08-19T13:07:00"))).toBe("1:07 PM");
  });

  it("styles ordinary messages as commands and results only for a verified agent contact", () => {
    expect(agentMessageClass(true, true)).toBe("agent-command");
    expect(agentMessageClass(true, false)).toBe("agent-result");
    expect(agentMessageClass(false, true)).toBe("");
    expect(agentMessageClass(false, false)).toBe("");
  });
});
