import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useLocation } from "@tanstack/react-router";
import { useSidebar } from "@/context/SidebarContext";
import { Users } from "lucide-react";
import {
  ScreenIcon,
  BagIcon,
  CloudUploadIcon, SolidScreenIcon, SolidCloudUploadIcon, SolidBagIcon, SolidBroadcastTower,
  BroadcastTower, Network, BullHornFilled, BullHorn,
} from "../../public/images/icons/theme-icons";

type NavItem = {
  name: string;
  icon: (active: boolean) => React.ReactNode;
  path?: string;
  subItems?: { name: string; path: string; pro?: boolean; new?: boolean }[];
  // adminOnly entries are hidden in the sidebar for member-role users.
  // The backend still gates the underlying routes regardless.
  adminOnly?: boolean;
};

const baseNavItems: NavItem[] = [
  {
    icon: (active) => active ? <SolidScreenIcon className="w-6 h-6" /> : <ScreenIcon className="w-6 h-6" />,
    name: "Screens",
    path: "/screen-list",
  },
  {
    icon: (active) => active ? <BullHornFilled className="w-6 h-6" /> : <BullHorn className="w-6 h-6" />,
    name: "Broadcasts",
    path: "/broadcast-list"
  },
  {
    icon: (active) => active ? <SolidCloudUploadIcon className="w-6 h-6" /> : <CloudUploadIcon className="w-6 h-6" />,
    name: "Media",
    path: "/content-list",
  },
  {
    icon: (active) => active ? <SolidBagIcon className="w-6 h-6" /> : <BagIcon className="w-6 h-6" />,
    name: "Apps",
    path: "/integrations"
  },
];

const AppSidebar: React.FC = () => {
  const { isMobileOpen } = useSidebar();
  const location = useLocation();
  const pathname = location.pathname;
  const navItems = baseNavItems;

  // Lightweight navigation progress indicator
  const [refreshing, setRefreshing] = useState(false);
  const startedAtRef = useRef<number | null>(null);

  // When the route changes, keep the bar for a minimum duration for a smooth feel
  useEffect(() => {
    if (!refreshing) return;
    const MIN_MS = 300;
    const now = Date.now();
    const startedAt = startedAtRef.current ?? now;
    const elapsed = now - startedAt;
    const remaining = Math.max(0, MIN_MS - elapsed);
    const t = setTimeout(() => {
      setRefreshing(false);
      startedAtRef.current = null;
    }, remaining);
    return () => clearTimeout(t);
  }, [pathname, refreshing]);

  const handleNavClick = useCallback(
    (targetPath?: string) => {
      if (!targetPath) return;
      if (targetPath !== pathname) {
        // Start progress bar immediately
        setRefreshing(true);
        if (!startedAtRef.current) startedAtRef.current = Date.now();
      }
    },
    [pathname]
  );

  const renderMenuItems = (
    navItems: NavItem[]
  ) => (
    <ul className="flex flex-col gap-4">
      {navItems.map((nav) => (
        <li key={nav.name}>
          {nav.path && (
              <Link
                to={nav.path}
                onClick={() => handleNavClick(nav.path)}
                className={`menu-item flex flex-col py-0.5 px-1 gap-0 group ${
                  isActive(nav.path) ? "menu-item-active" : "menu-item-inactive"
                }`}
              >
                <span
                  className={`${
                    isActive(nav.path)
                      ? "menu-item-icon-active"
                      : "menu-item-icon-inactive"
                  } menu-item-icon-wrapper`}
                >
                  {nav.icon(isActive(nav.path))}
                </span>
                <span className={`menu-item-text text-[8px]`}>{nav.name}</span>
              </Link>
          )}
        </li>
      ))}
    </ul>
  );

  // const isActive = (path: string) => path === pathname;
   const isActive = useCallback((path: string) => path === pathname, [pathname]);

  return (
    <aside
      className={`z-99999 fixed hidden sm:flex flex-col mt-0 top-0 px-3 left-0 text-gray-900 h-screen transition-all duration-300 ease-in-out
        w-18
        ${isMobileOpen ? "-translate-x-full" : "translate-x-0"}`}
    >
      {/* Slim progress bar at top when navigating */}
      {refreshing && (
        <div className="absolute top-0 left-0 right-0 h-0.5 overflow-hidden">
          <div className="h-full bg-primary-500 dark:bg-primary-400 animate-[progress_1.2s_ease-in-out_infinite]" />
        </div>
      )}

      <div
        className={`sm:flex hidden py-8 justify-center`}
      >
        <Link to="/" onClick={() => handleNavClick("/") }>
            <img
              src="/images/logo/logo-icon.svg"
              alt="Logo"
              className="dark:hidden"
              width={32}
              height={32}
            />
          <img
            src="/images/logo/logo-icon-dark.svg"
            alt="Logo"
            className="hidden dark:block"
            width={32}
            height={32}
          />
        </Link>
      </div>
      <div className="flex flex-col flex-1 overflow-y-auto duration-300 ease-linear no-scrollbar">
        <nav className="mb-6">
          <div className="flex flex-col gap-4">
            <div>
              {renderMenuItems(navItems)}
            </div>
          </div>
        </nav>
      </div>
    </aside>
  );
};

export default AppSidebar;
