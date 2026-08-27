import React, { useCallback, useEffect, useState } from "react";
import { Link, useLocation } from "@tanstack/react-router";
import { useLive, useRevision } from "@/ds";
import { fetchBroadcasts } from "@/utils/apps/api";
import { NAV_ITEMS, navActive } from "./nav-items";

/**
 * The nav carries live state.
 *
 * Whop puts a LIVE badge on the sidebar item itself, and for a fleet that is
 * the difference between a product you check and a product that tells you.
 * Nobody should have to navigate to Broadcasts to discover that something is
 * interrupting every screen they own.
 */
const AppSidebar: React.FC = () => {
  const location = useLocation();
  const pathname = location.pathname;
  const isOn = useCallback((path: string) => navActive(pathname, path), [pathname]);
  const revision = useRevision();
  const { attached } = useLive();
  const [onAir, setOnAir] = useState(0);

  useEffect(() => {
    let live = true;
    void fetchBroadcasts()
      .then((rows) => {
        if (!live) return;
        setOnAir(rows.filter((row) => row.cancelled_at_unix_ms == null).length);
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [revision]);

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
