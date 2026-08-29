import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { TanStackRouterVite } from '@tanstack/router-plugin/vite';
import path from 'path';

/**
 * The Signage application, served by a lait head.
 *
 * Like every World web client, this bundle is not compiled into the host. A
 * head serves it only from the selected immutable Signage release, and it
 * ships on the World's own update channel. `npm run build` emits straight
 * into `products/signage-app/assets/web`, which is **committed** and copied
 * into the release by `cargo stage-worlds` beside the runner and the signed
 * declaration — the same arrangement `viewer/` has with Issues. The same
 * command stages the unsealed tree a local World is added from.
 *
 * The tradeoff is honest: build output in git, kept fresh by `npm run build`
 * and guarded by CI diffing a rebuild.
 *
 * Dev runs two origins — vite on :3000, the engine on its own port — which
 * is exactly what `serve::auth` refuses. The proxy adapts (Host, Origin,
 * and the run token from the engine's `--json` line via LAIT_PORT/LAIT_TOKEN),
 * and production stays same-origin with no dev flag in the engine at all.
 */
export default defineConfig({
  plugins: [
    TanStackRouterVite({
      routesDirectory: './src/routes',
      generatedRouteTree: './src/routeTree.gen.ts',
    }),
    react(),
  ],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
    dedupe: ['react', 'react-dom'],
  },
  optimizeDeps: {
    include: [
      '@base-ui/react/popover',
      '@base-ui/react/context-menu',
      '@base-ui/react/dialog',
      '@base-ui/react/alert-dialog',
      '@use-gesture/react',
      'interactjs',
    ],
  },
  build: {
    outDir: '../assets/web',
    emptyOutDir: true,
    // No hashed filenames: the bundle is committed, so stable names keep the
    // diff legible and stop every rebuild from churning the tree with new
    // files. The World release is versioned as a whole, so cache-busting names
    // add nothing.
    rollupOptions: {
      output: {
        entryFileNames: 'app.js',
        chunkFileNames: '[name].js',
        assetFileNames: '[name][extname]',
      },
    },
  },
  server: {
    port: 3000,
    proxy: {
      '/api': {
        target: `http://127.0.0.1:${process.env.LAIT_PORT || '7717'}`,
        changeOrigin: true,
        ws: true,
        configure: (proxy) => {
          proxy.on('proxyReq', (proxyReq) => {
            proxyReq.removeHeader('origin');
            const token = process.env.LAIT_TOKEN;
            if (token) proxyReq.setHeader('authorization', `Bearer ${token}`);
          });
          proxy.on('proxyReqWs', (proxyReq) => {
            const port = process.env.LAIT_PORT || '7717';
            proxyReq.setHeader('origin', `http://127.0.0.1:${port}`);
            const token = process.env.LAIT_TOKEN;
            if (token) proxyReq.setHeader('authorization', `Bearer ${token}`);
          });
        },
      },
    },
  },
});
