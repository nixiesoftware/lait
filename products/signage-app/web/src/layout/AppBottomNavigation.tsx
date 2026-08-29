import React from "react";
import { Link, useLocation } from "@tanstack/react-router";
import { NAV_ITEMS, navActive } from "./nav-items";

const AppBottomNavigation: React.FC = () => {
  const pathname = useLocation().pathname;

  return (
    <nav className="ds-dock">
      {NAV_ITEMS.map((item) => {
        const active = navActive(pathname, item.path);
        return (
          <Link
            key={item.path}
            to={item.path}
            className={`ds-dock-item${active ? " is-on" : ""}`}
          >
            <item.Icon
              size={22}
              strokeWidth={active ? 2.4 : 1.7}
              absoluteStrokeWidth
            />
            {item.name}
          </Link>
        );
      })}
    </nav>
  );
};

export default AppBottomNavigation;
