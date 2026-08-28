import { createRootRoute, Outlet } from '@tanstack/react-router';
import { FocusProvider, ToastProvider } from '@/ds';

function RootComponent() {
  return (
    <ToastProvider>
      <FocusProvider>
        <Outlet />
      </FocusProvider>
    </ToastProvider>
  );
}

export const Route = createRootRoute({
  component: RootComponent,
});
