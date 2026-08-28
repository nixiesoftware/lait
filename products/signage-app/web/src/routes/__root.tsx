import { createRootRoute, Outlet } from '@tanstack/react-router';
import { ToastProvider } from '@/ds';

function RootComponent() {
  return (
    <ToastProvider>
      <Outlet />
    </ToastProvider>
  );
}

export const Route = createRootRoute({
  component: RootComponent,
});
