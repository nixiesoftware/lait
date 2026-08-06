import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // The keymap is a DOM contract — `KeyboardEvent.key` semantics are the thing
    // under test, so a real DOM implementation is the point, not a convenience.
    environment: "jsdom",
    // Stubs for the platform APIs jsdom lacks and the design system uses —
    // popover, matchMedia, ResizeObserver. See the file for what each is and
    // what it deliberately does not pretend to do.
    setupFiles: ["./vitest.setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
