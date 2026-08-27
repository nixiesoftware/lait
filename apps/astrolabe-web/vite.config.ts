import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

/**
 * This app stays a normal browser application. The future desktop shell opens
 * the loopback URL that lait or lait-workbench announces; Vite only serves the
 * isolated UI while developing it.
 */
export default defineConfig({
  plugins: [react()],
  server: {
    // Exactly what `src-tauri/tauri.conf.json` names as `devUrl`, and both
    // halves matter.
    //
    // `host`: without it Vite binds `[::1]` only on a dual-stack machine, while
    // Tauri probes `127.0.0.1` -- so `tauri dev` prints "Waiting for your
    // frontend dev server" forever and the window is a white pane. Nothing
    // errors; the two sides simply never meet.
    //
    // `strictPort`: without it a busy 5180 makes Vite move to 5181 and say so
    // in a log nobody is watching, while Tauri keeps waiting on 5180. Same
    // silent hang, one port over. Refusing to start is the version of that
    // failure a person can act on.
    host: "127.0.0.1",
    port: 5180,
    strictPort: true,
  },
  test: {
    environment: "jsdom",
  },
});
