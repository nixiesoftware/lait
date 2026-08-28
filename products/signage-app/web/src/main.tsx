import React from 'react';
import ReactDOM from 'react-dom/client';
import { RouterProvider, createRouter } from '@tanstack/react-router';
import { LiveProvider } from './ds/live';
import { routeTree } from './routeTree.gen';
import './ds/tokens.css';
import './ds/signage.css';
import './ds/base.css';
import './ds/controls.css';
import './ds/language.css';
import './ds/overlay.css';
import './ds/page.css';
import './ds/instruments.css';
import './ds/gesture.css';

const router = createRouter({ routeTree });

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router;
  }
}

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    {/* One clock and one doorbell for the whole app: a hundred rows each
        holding an interval is a hundred clocks disagreeing by a frame. */}
    <LiveProvider>
      <RouterProvider router={router} />
    </LiveProvider>
  </React.StrictMode>,
);
