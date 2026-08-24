import { createFileRoute, Outlet, useLocation } from '@tanstack/react-router';
import AppSidebar from '@/layout/AppSidebar';
import AppBottomNavigation from '@/layout/AppBottomNavigation';
import {
  AdminLayoutProvider,
  useAdminLayout,
} from '@/context/AdminLayoutContext';
import {
  BroadcastGradientProvider,
  useBroadcastGradient,
} from '@/context/BroadcastGradientContext';

function AdminLayoutContent() {
  const { hideSidebar } = useAdminLayout();
  useBroadcastGradient();
  const location = useLocation();
  const isEditorRoute =
    location.pathname?.startsWith('/broadcast-list/broadcast/') ?? false;

  return (
    <div className="ds-shell">
      {!hideSidebar && (
        <>
          <AppSidebar />
          <AppBottomNavigation />
        </>
      )}
      <div className="ds-main">
        <div className={`ds-main-body${isEditorRoute ? " is-editor" : ""}`}>
          <Outlet />
        </div>
      </div>
    </div>
  );
}

function AdminLayout() {
  return (
    <AdminLayoutProvider>
      <BroadcastGradientProvider>
        <AdminLayoutContent />
      </BroadcastGradientProvider>
    </AdminLayoutProvider>
  );
}

export const Route = createFileRoute('/_admin')({
  component: AdminLayout,
});
