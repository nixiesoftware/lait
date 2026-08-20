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
    port: 5180,
  },
  test: {
    environment: "jsdom",
  },
});
