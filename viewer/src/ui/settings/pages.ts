/**
 * The settings taxonomy — every sub-page, the group it sits in, and the words
 * a person might type to find it.
 *
 * One list drives three things: the rail (grouped, in this order), the search
 * box above it (label and keywords), and the route's idea of a valid `?tab=`
 * (which `route.ts` spells separately and `Settings.tsx` narrows against). It
 * is a list, not a tree, because Linear's rail is a list with headings and
 * that is the shape a person scans.
 *
 * Keywords are the field labels on the page — "theme" finds Preferences the
 * way it does in Linear, where the search box answers to a row, not only to a
 * page name.
 */

export type SettingsTab =
  | "preferences"
  | "profile"
  | "notifications"
  | "general"
  | "members"
  | "teams"
  | "access"
  | "devices"
  | "labels"
  | "workflow";

export type SettingsGroup = "Personal" | "Issues" | "Administration";

export interface SettingsPage {
  tab: SettingsTab;
  label: string;
  group: SettingsGroup;
  /** What the search box also answers to. Lower-case, one phrase each. */
  keywords: readonly string[];
}

export const SETTINGS_PAGES: readonly SettingsPage[] = [
  {
    tab: "preferences",
    label: "Preferences",
    group: "Personal",
    keywords: [
      "theme",
      "dark mode",
      "light mode",
      "appearance",
      "density",
      "compact",
      "comfortable",
      "home view",
      "default view",
      "first day of the week",
      "week start",
      "comments",
      "enter",
      "keyboard shortcuts",
    ],
  },
  {
    tab: "profile",
    label: "Profile",
    group: "Personal",
    keywords: ["name", "identity", "actor", "device", "role", "capabilities", "did", "key"],
  },
  {
    tab: "notifications",
    label: "Notifications",
    group: "Personal",
    keywords: ["inbox", "assignments", "comments", "mentions", "status changes", "snoozed", "grouping"],
  },
  { tab: "labels", label: "Labels", group: "Issues", keywords: ["tags", "colour", "color"] },
  {
    tab: "workflow",
    label: "Workflow",
    group: "Issues",
    keywords: ["statuses", "states", "columns", "backlog", "done", "in progress"],
  },
  {
    tab: "general",
    label: "General",
    group: "Administration",
    keywords: ["space name", "description", "identity", "danger zone", "forget", "rotate key"],
  },
  {
    tab: "members",
    label: "Members",
    group: "Administration",
    keywords: ["people", "invite", "invite link", "agents", "sponsor", "access log", "remove"],
  },
  {
    tab: "teams",
    label: "Teams",
    group: "Administration",
    keywords: ["team", "create team", "identifier", "ownership"],
  },
  {
    tab: "access",
    label: "Roles & access",
    group: "Administration",
    keywords: ["roles", "grants", "permissions", "capabilities", "revoke"],
  },
  {
    tab: "devices",
    label: "Devices & recovery",
    group: "Administration",
    keywords: ["sessions", "enrol", "enrollment token", "custody", "recovery share", "passphrase", "backup"],
  },
];

export const SETTINGS_GROUPS: readonly SettingsGroup[] = ["Personal", "Issues", "Administration"];

const TABS = new Set<string>(SETTINGS_PAGES.map((page) => page.tab));

export function isSettingsTab(value: string | null | undefined): value is SettingsTab {
  return value !== null && value !== undefined && TABS.has(value);
}

export interface SettingsMatch {
  page: SettingsPage;
  /** The keyword that matched, when the label did not. */
  via: string | null;
}

/**
 * Pages answering to `query`, label matches first, then keyword matches in
 * rail order. Empty query → every page, in rail order, with no `via`.
 */
export function searchSettings(query: string): SettingsMatch[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return SETTINGS_PAGES.map((page) => ({ page, via: null }));
  const byLabel: SettingsMatch[] = [];
  const byKeyword: SettingsMatch[] = [];
  for (const page of SETTINGS_PAGES) {
    if (page.label.toLowerCase().includes(needle)) {
      byLabel.push({ page, via: null });
      continue;
    }
    const via = page.keywords.find((keyword) => keyword.includes(needle));
    if (via) byKeyword.push({ page, via });
  }
  return [...byLabel, ...byKeyword];
}
