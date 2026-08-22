import { createFileRoute, Outlet, useLocation } from '@tanstack/react-router';
import AppSidebar from '@/layout/AppSidebar';
import AppBottomNavigation from '@/layout/AppBottomNavigation';
import Backdrop from '@/layout/Backdrop';
import Navbar from '@/components/common/Navbar';
import React from 'react';
import {
  AdminLayoutProvider,
  useAdminLayout,
} from '@/context/AdminLayoutContext';
import {
  BroadcastGradientProvider,
  useBroadcastGradient,
} from '@/context/BroadcastGradientContext';
import { AnimatePresence, motion } from 'framer-motion';

function AdminLayoutContent() {
  const { hideSidebar } = useAdminLayout();
  useBroadcastGradient();
  const location = useLocation();
  const pathname = location.pathname;
  const isEditorRoute =
    pathname?.startsWith('/broadcast-list/broadcast/') ?? false;

  const editorBackgroundStyle = React.useMemo(() => {
    if (!isEditorRoute) return {};
    return {
      backgroundImage: `linear-gradient(135deg, #0b0c1a 0%, #0b1230 40%, #111d3d 100%)`,
    } as React.CSSProperties;
  }, [isEditorRoute]);

  return (
    <div
      className={`
        relative h-screen overflow-hidden flex transition animate-all duration-100 ease-[cubic-bezier(.94,-0.74,0,-0.01)]
      `}
    >
      <div className="pointer-events-none absolute inset-0 -z-10">
        <div className="absolute inset-0 normal-background" />
        <AnimatePresence initial={false} mode="wait">
          {isEditorRoute && (
            <motion.div
              key="broadcast-bg"
              className="absolute inset-0 will-change-transform"
              style={editorBackgroundStyle}
              initial={{ filter: 'blur(30px)', scale: 3, opacity: 0.6 }}
              animate={{ filter: 'blur(0px)', scale: 1, opacity: 1 }}
              exit={{ filter: 'blur(30px)', scale: 3, opacity: 0.0 }}
              transition={{ duration: 0.6, ease: [0.4, 0.0, 0.2, 1] }}
            />
          )}
        </AnimatePresence>
      </div>

      {!hideSidebar && (
        <>
          <AppSidebar />
          <AppBottomNavigation />
          <Backdrop />
        </>
      )}
      <div
        className={`relative flex flex-col flex-1 min-w-0 sm:my-3 sm:mr-3
          ${
            isEditorRoute
              ? 'min-md:ml-3 max-sm:mt-[calc(100vh-250px)]'
              : 'sm:ml-18'
          }
          animate-all duration-300 ease-[cubic-bezier(.12,.88,.68,1.11)]`}
      >
        {!isEditorRoute && <div className="hidden sm:block"><Navbar /></div>}
        <div
          className={`relative flex-1 sm:rounded-lg no-scrollbar overflow-x-hidden overflow-y-auto px-3 sm:px-6 sm:py-4
            ${
              isEditorRoute
                ? 'bg-white/0 border-0 border-none'
                : 'bg-white dark:bg-gray-900 border-1 border-gray-300 dark:border-gray-800'
            }
            min-h-0 min-md:rounded-sm
          `}
        >
          <Outlet />
          {/* Spacer for mobile bottom navigation */}
          <div className="h-16 sm:hidden" />
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
