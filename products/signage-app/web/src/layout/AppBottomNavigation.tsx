import React from "react";
import { Link, useLocation } from "@tanstack/react-router";
import { Moon } from "lucide-react";
import {
  BagIcon,
  BullHorn,
  BullHornFilled,
  CloudUploadIcon,
  ScreenIcon,
  SolidBagIcon,
  SolidCloudUploadIcon,
  SolidScreenIcon,
} from "../../public/images/icons/theme-icons";

const navItems = [
  {
    name: "Screens",
    path: "/screen-list",
    icon: (active: boolean) =>
      active ? (
        <SolidScreenIcon className="w-6 h-6" fill="currentColor" />
      ) : (
        <ScreenIcon className="w-6 h-6" />
      ),
  },
  {
    name: "Broadcasts",
    path: "/broadcast-list",
    icon: (active: boolean) =>
      active ? (
        <BullHornFilled className="w-6 h-6" fill="currentColor" />
      ) : (
        <BullHorn className="w-6 h-6" />
      ),
  },
  {
    name: "Media",
    path: "/content-list",
    icon: (active: boolean) =>
      active ? (
        <SolidCloudUploadIcon className="w-6 h-6" fill="currentColor" />
      ) : (
        <CloudUploadIcon className="w-6 h-6" />
      ),
  },
  {
    name: "Apps",
    path: "/integrations",
    icon: (active: boolean) =>
      active ? (
        <SolidBagIcon className="w-6 h-6" fill="currentColor" />
      ) : (
        <BagIcon className="w-6 h-6" />
      ),
  },
];

const AppBottomNavigation: React.FC = () => {
  const location = useLocation();
  const pathname = location.pathname;

  const isActive = (path: string) => {
    if (path === "/" && pathname === "/") return true;
    return path !== "/" && pathname.startsWith(path);
  };

  const toggleTheme = () => {
    const isDark = document.documentElement.classList.contains("dark");
    document.documentElement.classList.toggle("dark");
    localStorage.setItem("theme", isDark ? "light" : "dark");
  };

  return (
    <nav className="fixed bottom-0 left-0 right-0 bg-white dark:bg-gray-900 z-50 sm:hidden">
      <div className="flex items-center justify-around h-16 px-2 border-t border-gray-200 dark:border-gray-800">
        {navItems.map((item) => {
          const active = isActive(item.path);
          return (
            <Link
              key={item.path}
              to={item.path}
              className={`flex flex-col items-center justify-center flex-1 py-1.5 transition-colors ${
                active
                  ? "text-brand-500 dark:text-brand-400"
                  : "text-gray-500 dark:text-gray-400"
              }`}
            >
              {item.icon(active)}
              <span className="text-[10px] mt-0.5">{item.name}</span>
            </Link>
          );
        })}

        <button
          onClick={toggleTheme}
          className="flex flex-col items-center justify-center flex-1 py-1.5 text-gray-500 dark:text-gray-400 transition-colors"
        >
          <Moon className="w-6 h-6" />
          <span className="text-[10px] mt-0.5">Theme</span>
        </button>
      </div>
    </nav>
  );
};

export default AppBottomNavigation;
