import type { LucideIcon } from "lucide-react";
import { Clapperboard, Images, Monitor, Radio, Tv } from "lucide-react";

export type NavItem = {
  name: string;
  path: string;
  Icon: LucideIcon;
};

/**
 * Ordered from the glass down to the source, which is the order resolution
 * ranks them: a broadcast outranks a channel, a channel carries programs, a
 * program is made of files. The rail is the ladder.
 */
export const NAV_ITEMS: NavItem[] = [
  { name: "Screens", path: "/screen-list", Icon: Monitor },
  { name: "Broadcasts", path: "/broadcast-hub", Icon: Radio },
  { name: "Channels", path: "/channel-list", Icon: Tv },
  { name: "Programs", path: "/broadcast-list", Icon: Clapperboard },
  { name: "Files", path: "/content-list", Icon: Images },
];

export function navActive(pathname: string, path: string): boolean {
  return pathname === path || pathname.startsWith(`${path}/`);
}
