import React, { useCallback } from "react";
import { Link, useLocation } from "@tanstack/react-router";
import { NAV_ITEMS, navActive } from "./nav-items";

const AppSidebar: React.FC = () => {
  const location = useLocation();
  const pathname = location.pathname;
  const isOn = useCallback((path: string) => navActive(pathname, path), [pathname]);

  return (
    <aside className="ds-nav">
      <Link to="/" className="ds-nav-logo" aria-label="Home">
        <img src="/images/logo/logo-icon-dark.svg" alt="" width={28} height={28} />
      </Link>
      <nav>
        <ul className="ds-nav-list">
          {NAV_ITEMS.map((nav) => {
            const active = isOn(nav.path);
            return (
              <li key={nav.path}>
                <Link
                  to={nav.path}
                  className={`ds-nav-item${active ? " is-on" : ""}`}
                >
                  <nav.Icon
                    size={22}
                    strokeWidth={active ? 2.4 : 1.7}
                    absoluteStrokeWidth
                  />
                  {nav.name}
                </Link>
              </li>
            );
          })}
        </ul>
      </nav>
    </aside>
  );
};

export default AppSidebar;
