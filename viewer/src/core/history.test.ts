import { beforeEach, describe, expect, it } from "vitest";

import { here, leave, openedHere, push, replace } from "./history";

/**
 * jsdom gives these tests a real `window.history`, which is what makes them
 * worth writing: the bugs being pinned here are about what `pushState` and
 * `replaceState` actually do to the stack and to `history.state`, and a mock of
 * those two functions would pass whether or not the reasoning was right.
 *
 * `history.back()` is asynchronous even in jsdom, so `leave` is asserted on the
 * decision it makes rather than on where the browser lands. Where it lands is
 * the browser's business; whether it should have gone at all is ours.
 */
describe("the viewer's history verbs", () => {
  beforeEach(() => {
    window.history.replaceState(null, "", "/spaces/ws_x/projects/ENG/issues");
  });

  it("reads the address the way formatRoute writes it", () => {
    window.history.replaceState(null, "", "/spaces/ws_x/projects/ENG/issues?issue=iss_1");
    expect(here()).toBe("/spaces/ws_x/projects/ENG/issues?issue=iss_1");
  });

  /**
   * The dead-Back bug, in one assertion.
   *
   * Every navigation verb pushed unconditionally, so re-picking the lit tab,
   * the current sidebar entry or the milestone already in the filter each added
   * an entry for a page you were already on. Measured on the running head,
   * three clicks of the current Issues tab took `history.length` from 2 to 5 —
   * so Back had to be pressed three times before anything moved.
   */
  it("refuses to stack the address already showing", () => {
    const depth = window.history.length;
    expect(push("/spaces/ws_x/projects/ENG/issues")).toBe(false);
    expect(push("/spaces/ws_x/projects/ENG/issues")).toBe(false);
    expect(window.history.length).toBe(depth);

    expect(push("/spaces/ws_x/projects/ENG/board")).toBe(true);
    expect(here()).toBe("/spaces/ws_x/projects/ENG/board");
  });

  it("counts the query, so opening an issue is a different address", () => {
    expect(push("/spaces/ws_x/projects/ENG/issues?issue=iss_1")).toBe(true);
    expect(here()).toBe("/spaces/ws_x/projects/ENG/issues?issue=iss_1");
  });

  it("marks only the entries that stand a document over a surface", () => {
    push("/spaces/ws_x/projects/ENG/board");
    expect(openedHere("issue")).toBe(false);
    push("/spaces/ws_x/projects/ENG/board?issue=iss_1", "issue");
    expect(openedHere("issue")).toBe(true);
    // The kinds do not answer for each other: a Spec's register is not an
    // issue's list, and closing one must not go back through the other.
    expect(openedHere("spec")).toBe(false);
  });

  /**
   * The regression that made every other fix here worthless.
   *
   * The address-sync effect replaced on every selection change and passed
   * `null` for the state, which erased the marker. So opening an issue and then
   * moving the cursor — or letting a board refresh repair the selection — left
   * an entry that had been pushed for a document but no longer said so, and
   * closing it fell back to rewriting the address in place: the list you came
   * from was spent, exactly as before.
   */
  it("keeps the entry's state through a replace", () => {
    push("/spaces/ws_x/projects/ENG/issues?issue=iss_1", "issue");
    expect(replace("/spaces/ws_x/projects/ENG/issues?issue=iss_2")).toBe(true);
    expect(openedHere("issue")).toBe(true);
    expect(here()).toBe("/spaces/ws_x/projects/ENG/issues?issue=iss_2");
  });

  it("does not replace the address already showing either", () => {
    expect(replace("/spaces/ws_x/projects/ENG/issues")).toBe(false);
  });

  describe("closing a document", () => {
    it("goes back when the entry is one it pushed", () => {
      push("/spaces/ws_x/projects/ENG/issues?issue=iss_1", "issue");
      expect(leave("issue")).toBe(true);
    });

    /**
     * A deep link straight into an issue has nothing behind it but the page
     * load, so there is no surface to return to and `leave` says so — the
     * caller then closes the document in place. Going back regardless would
     * take the person out of the app entirely, which is the worst possible
     * reading of a close button.
     */
    it("declines on an entry it did not push", () => {
      window.history.replaceState(null, "", "/spaces/ws_x/projects/ENG/issues?issue=iss_1");
      expect(leave("issue")).toBe(false);
    });

    it("declines when the entry belongs to a different kind of document", () => {
      push("/spaces/ws_x/projects/ENG/specs?spec=spc_1", "spec");
      expect(leave("issue")).toBe(false);
    });
  });
});
