/**
 * jsdom gaps the design system depends on.
 *
 * These are not conveniences. Astryx builds on modern platform APIs — the
 * native popover API, container queries, `matchMedia` for its responsive and
 * reduced-motion decisions — and jsdom implements none of them. Without stubs
 * the component throws before the behaviour under test ever runs, which turns a
 * missing browser API into a failing assertion about our own code.
 *
 * Each stub is the smallest thing that lets the component mount. None of them
 * pretend to implement the feature: `matchMedia` always answers "no", so a test
 * asserting responsive behaviour would still be asserting nothing, and should
 * say so rather than lean on this file.
 */

if (typeof window !== "undefined") {
  if (!window.matchMedia) {
    window.matchMedia = ((query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    })) as typeof window.matchMedia;
  }

  // cmdk measures its list; Astryx's layers observe their anchor.
  if (!window.ResizeObserver) {
    window.ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    } as unknown as typeof window.ResizeObserver;
  }

  // The popover API backs every Astryx overlay. jsdom parses the attribute but
  // implements neither method, and `:popover-open` is not a selector it knows —
  // which is why tests scope through `aria-controls` instead.
  const proto = window.HTMLElement.prototype as HTMLElement & {
    showPopover?: () => void;
    hidePopover?: () => void;
  };
  if (!proto.showPopover) proto.showPopover = function () {};
  if (!proto.hidePopover) proto.hidePopover = function () {};

  // `CSS.escape`, which Astryx's `Dialog` calls to build the selector for its
  // own accessible-name guardrail. jsdom has no `CSS` namespace at all, so the
  // guardrail throws instead of warning. The stub quotes the identifier the way
  // the spec's serialisation does for the only shape that reaches it here — a
  // generated id — rather than implementing the algorithm.
  const cssNamespace = (globalThis as { CSS?: { escape?: (value: string) => string } }).CSS;
  if (!cssNamespace) {
    (globalThis as { CSS?: unknown }).CSS = {
      escape: (value: string) => String(value).replace(/([^\w-])/g, "\\$1"),
    };
  } else if (!cssNamespace.escape) {
    cssNamespace.escape = (value: string) => String(value).replace(/([^\w-])/g, "\\$1");
  }

  // `<dialog>` is the same gap one element down: jsdom constructs
  // `HTMLDialogElement` but ships none of its methods, so Astryx's `Dialog`
  // throws `showModal is not a function` in its mount effect — before any
  // component under test has rendered a line. The stubs track `open` because
  // that is the one piece of state a test can meaningfully assert; nothing here
  // implements the top layer, focus trapping or light dismiss.
  const dialogProto = window.HTMLDialogElement?.prototype as HTMLDialogElement | undefined;
  if (dialogProto && !dialogProto.showModal) {
    dialogProto.showModal = function (this: HTMLDialogElement) {
      this.open = true;
    };
    dialogProto.show = function (this: HTMLDialogElement) {
      this.open = true;
    };
    dialogProto.close = function (this: HTMLDialogElement) {
      this.open = false;
      this.dispatchEvent(new Event("close"));
    };
  }
}
