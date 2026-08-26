import { describe, expect, it } from "vitest";

import { RESTATED, applyWindowControls, declaredControls, trackWindowControls } from "./windowControls";

/** A stand-in for `document.documentElement`'s style declaration. */
function element() {
  const written: Record<string, string> = {};
  return {
    written,
    style: {
      setProperty(name: string, value: string) {
        written[name] = value;
      },
    },
  };
}

describe("declaredControls", () => {
  it("reads what the host declared", () => {
    expect(declaredControls({ __LAIT_WINDOW_CONTROLS__: { top: 28, leading: 78 } })).toEqual({
      top: 28,
      leading: 78,
    });
  });

  it("is absent in a browser tab, where nothing declares anything", () => {
    expect(declaredControls({})).toBeNull();
  });

  // The interesting half. Every one of these would otherwise reach CSS as a
  // padding value, and a shell inset by `NaN` names nothing about the host that
  // sent it.
  it.each([
    ["a string", { top: "28", leading: "78" }],
    ["a missing half", { top: 28 }],
    ["a negative inset", { top: -28, leading: 78 }],
    ["an inset past the ceiling", { top: 28, leading: 4000 }],
    ["NaN", { top: Number.NaN, leading: 78 }],
    ["Infinity", { top: Number.POSITIVE_INFINITY, leading: 78 }],
    ["not an object", "overlay"],
    ["null", null],
  ])("refuses %s", (_name, declared) => {
    expect(declaredControls({ __LAIT_WINDOW_CONTROLS__: declared })).toBeNull();
  });

  it("folds an all-zero declaration into the same absence it means", () => {
    expect(declaredControls({ __LAIT_WINDOW_CONTROLS__: { top: 0, leading: 0 } })).toBeNull();
  });
});

describe("applyWindowControls", () => {
  it("publishes the declaration as pixels", () => {
    const root = element();
    applyWindowControls(root, { top: 28, leading: 78 });
    expect(root.written).toEqual({
      "--window-controls-top": "28px",
      "--window-controls-leading": "78px",
    });
  });

  // Not "leaves them unset": every surface reads these, so the absence has to
  // arrive as a length rather than as an empty custom property that turns
  // `padding-top` into a parse error.
  it("writes zeroes when there is no declaration", () => {
    const root = element();
    applyWindowControls(root, null);
    expect(root.written).toEqual({
      "--window-controls-top": "0px",
      "--window-controls-leading": "0px",
    });
  });
});

/** A fake window that carries the global and the one event the host rings. */
function scope(controls: unknown) {
  const listeners: Array<() => void> = [];
  return {
    __LAIT_WINDOW_CONTROLS__: controls,
    addEventListener(type: string, listener: () => void) {
      if (type === RESTATED) listeners.push(listener);
    },
    removeEventListener(type: string, listener: () => void) {
      if (type === RESTATED) listeners.splice(listeners.indexOf(listener), 1);
    },
    restate(next: unknown) {
      this.__LAIT_WINDOW_CONTROLS__ = next;
      for (const listener of [...listeners]) listener();
    },
    get listening() {
      return listeners.length;
    },
  };
}

describe("trackWindowControls", () => {
  it("applies what is declared before anything is restated", () => {
    const root = element();
    trackWindowControls(root, scope({ top: 28, leading: 78 }));
    expect(root.written["--window-controls-top"]).toBe("28px");
  });

  // Full screen: the controls leave the page, and a shell still holding room
  // for them wears a band of nothing along its top edge.
  it("gives the room back when the host says the controls are gone", () => {
    const root = element();
    const host = scope({ top: 28, leading: 78 });
    trackWindowControls(root, host);
    host.restate(null);
    expect(root.written).toEqual({
      "--window-controls-top": "0px",
      "--window-controls-leading": "0px",
    });
  });

  it("takes the room back again on the way out of full screen", () => {
    const root = element();
    const host = scope(null);
    trackWindowControls(root, host);
    expect(root.written["--window-controls-top"]).toBe("0px");
    host.restate({ top: 28, leading: 78 });
    expect(root.written["--window-controls-top"]).toBe("28px");
  });

  it("stops listening when told to", () => {
    const root = element();
    const host = scope({ top: 28, leading: 78 });
    trackWindowControls(root, host)();
    expect(host.listening).toBe(0);
    host.restate(null);
    expect(root.written["--window-controls-top"]).toBe("28px");
  });
});
