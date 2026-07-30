import { describe, expect, it } from "vitest";

import {
  bridgeProtocolVersion,
  decodeFrame,
  lane,
  maxBridgeFrameBytes,
} from "./bridge";

/** postcard varint(u32), which is what the engine writes for every length and
 *  every number in these frames. */
function varint(value: number): number[] {
  const out: number[] = [];
  let rest = value >>> 0;
  while (rest >= 0x80) {
    out.push((rest & 0x7f) | 0x80);
    rest >>>= 7;
  }
  out.push(rest);
  return out;
}

function str(value: string): number[] {
  const bytes = Array.from(new TextEncoder().encode(value));
  return [...varint(bytes.length), ...bytes];
}

function progressBody(p: {
  transfer: string;
  content: string;
  moved: number;
  total: number;
  done: boolean;
}): number[] {
  return [
    ...str(p.transfer),
    ...str(p.content),
    ...varint(p.moved),
    ...varint(p.total),
    p.done ? 1 : 0,
  ];
}

function frame(laneId: number, body: number[], version = bridgeProtocolVersion): Uint8Array {
  return new Uint8Array([...varint(version), ...varint(laneId), ...varint(body.length), ...body]);
}

describe("bridge frames", () => {
  it("decodes a progress frame the engine would send", () => {
    const body = progressBody({
      transfer: "t-1",
      content: "abc",
      moved: 300,
      total: 1000,
      done: false,
    });
    expect(decodeFrame(frame(lane.progress, body))).toEqual({
      kind: "progress",
      progress: {
        transfer: "t-1",
        content: "abc",
        moved: 300,
        total: 1000,
        done: false,
      },
    });
  });

  it("refuses a frame past the ceiling before reading it", () => {
    // The order is the point: a decoder told a message is large has already
    // done the allocation by the time it fails.
    const oversize = new Uint8Array(maxBridgeFrameBytes + 1);
    expect(decodeFrame(oversize)).toBeNull();
  });

  it("refuses a body that claims to be larger than the frame carrying it", () => {
    const lying = new Uint8Array([
      ...varint(bridgeProtocolVersion),
      ...varint(lane.progress),
      ...varint(9999),
      1,
      2,
      3,
    ]);
    expect(decodeFrame(lying)).toBeNull();
  });

  it("ignores a lane it does not know rather than guessing", () => {
    // A lane this build does not implement is a lane a later build added. It is
    // not an error and it is not something to interpret.
    expect(decodeFrame(frame(7, [1, 2, 3]))).toBeNull();
    expect(decodeFrame(frame(lane.control, [1, 2, 3]))).toBeNull();
  });

  it("refuses a frame from another build", () => {
    const body = progressBody({
      transfer: "t",
      content: "c",
      moved: 1,
      total: 2,
      done: true,
    });
    expect(decodeFrame(frame(lane.progress, body, bridgeProtocolVersion + 1))).toBeNull();
  });

  it("does not throw on garbage", () => {
    for (const bytes of [
      new Uint8Array([]),
      new Uint8Array([0xff, 0xff, 0xff, 0xff, 0xff, 0xff]),
      new Uint8Array([1, 1, 200, 1, 2]),
    ]) {
      expect(() => decodeFrame(bytes)).not.toThrow();
    }
  });

  it("reads a multi-byte varint the way postcard writes one", () => {
    // A transfer past 127 bytes is the common case, not an edge one.
    const body = progressBody({
      transfer: "t",
      content: "c",
      moved: 300_000,
      total: 1_048_576,
      done: false,
    });
    const decoded = decodeFrame(frame(lane.progress, body));
    expect(decoded).toMatchObject({
      kind: "progress",
      progress: { moved: 300_000, total: 1_048_576 },
    });
  });
});
