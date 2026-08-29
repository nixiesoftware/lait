import React, { useCallback } from "react";
import { Link, useLocation } from "@tanstack/react-router";
import { useLive } from "@/ds";
import { useFleet } from "@/utils/screens/fleet";
import { NAV_ITEMS, navActive } from "./nav-items";

/**
 * The nav carries live state.
 *
 * Whop puts a LIVE badge on the sidebar item itself, and for a fleet that is
 * the difference between a product you check and a product that tells you.
 * Nobody should have to navigate to Broadcasts to discover that something is
 * interrupting every screen they own. It reads the same held copy every page
 * reads, so it never disagrees with the page beside it.
 */
const AppSidebar: React.FC = () => {
  const location = useLocation();
  const pathname = location.pathname;
  const isOn = useCallback((path: string) => navActive(pathname, path), [pathname]);
  const { attached, now } = useLive();
  const { broadcasts } = useFleet();
  const onAir = broadcasts.filter(
    (row) => row.cancelled_at_unix_ms == null || now < row.cancelled_at_unix_ms,
  ).length;

  return (
    <aside className="ds-nav">
      <Link to="/" className="ds-nav-logo" aria-label="Home">
        <img src="/images/logo/logo-icon.svg" alt="" width={28} height={28} />
      </Link>
      <nav>
        <ul className="ds-nav-list">
          {NAV_ITEMS.map((nav) => {
            const active = isOn(nav.path);
            const live = nav.path === "/broadcast-hub" && onAir > 0;
            return (
              <li key={nav.path}>
                <Link to={nav.path} className={`ds-nav-item${active ? " is-on" : ""}`}>
                  <nav.Icon
                    size={22}
                    strokeWidth={active ? 2.4 : 1.7}
                    absoluteStrokeWidth
                  />
                  {nav.name}
                  {live && (
                    <span
                      className="ds-nav-live"
                      title={`${onAir} broadcast${onAir === 1 ? "" : "s"} on air`}
                    />
                  )}
                </Link>
              </li>
            );
          })}
        </ul>
      </nav>
      {/*
        Not being told is not the same as nothing happening. If the doorbell is
        detached, everything on screen may be stale and looking calm — so say
        so, quietly, rather than letting silence read as settled.
      */}
      {!attached && (
        <span className="ds-detached" title="Reconnecting to live updates">
          <span className="ds-nav-live" style={{ background: "var(--ds-miss)" }} />
          offline
        </span>
      )}
    </aside>
  );
};

export default AppSidebar;
