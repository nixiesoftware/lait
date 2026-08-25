import type { LucideIcon } from "lucide-react";
import { Clapperboard, Images, Monitor, Radio } from "lucide-react";

export type NavItem = {
  name: string;
  path: string;
  Icon: LucideIcon;
};

export const NAV_ITEMS: NavItem[] = [
  { name: "Screens", path: "/screen-list", Icon: Monitor },
  { name: "Programs", path: "/broadcast-list", Icon: Clapperboard },
  { name: "Broadcasts", path: "/broadcast-hub", Icon: Radio },
  { name: "Media", path: "/content-list", Icon: Images },
];

export function navActive(pathname: string, path: string): boolean {
  return pathname === path || pathname.startsWith(`${path}/`);
}
