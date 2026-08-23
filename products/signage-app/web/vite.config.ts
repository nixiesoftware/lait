import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import svgr from 'vite-plugin-svgr';
import { TanStackRouterVite } from '@tanstack/router-plugin/vite';
import path from 'path';

/**
 * The Signage application, served by a lait head (`lait --world signage`).
 *
 * Like every World web client, this bundle is not compiled into the host. A
 * head serves it only from the selected immutable Signage release, and it
 * ships on the World's own update channel. `npm run build` emits `dist/`;
 * staging that into a release directory is the dev loop.
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
    svgr({
      include: '**/*.svg',
    }),
  ],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
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
