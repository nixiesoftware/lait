import { createRootRoute, Outlet } from '@tanstack/react-router';
import { FocusProvider, ToastProvider, UndoProvider } from '@/ds';

function RootComponent() {
  return (
    <ToastProvider>
      <UndoProvider>
        <FocusProvider>
          <Outlet />
        </FocusProvider>
      </UndoProvider>
    </ToastProvider>
  );
}

export const Route = createRootRoute({
  component: RootComponent,
});
