import { Toast } from "@base-ui/react/toast";
import type { ReactNode } from "react";

export function ToastProvider({ children }: { children: ReactNode }) {
  return (
    <Toast.Provider>
      {children}
      <Toast.Portal>
        <Toast.Viewport className="ds-toast-viewport">
          <ToastList />
        </Toast.Viewport>
      </Toast.Portal>
    </Toast.Provider>
  );
}

function ToastList() {
  const { toasts } = Toast.useToastManager();
  return toasts.map((entry) => (
    <Toast.Root key={entry.id} toast={entry} className="ds-toast">
      <Toast.Content className="ds-toast-body">
        <Toast.Title className="ds-toast-title" />
        <Toast.Description className="ds-toast-copy" />
      </Toast.Content>
      <Toast.Close className="ds-icon" aria-label="Dismiss">
        ×
      </Toast.Close>
    </Toast.Root>
  ));
}

export function useToast() {
  const manager = Toast.useToastManager();
  return {
    show: (title: string, description?: string) => {
      manager.add({ title, description, timeout: 2800 });
    },
  };
}
