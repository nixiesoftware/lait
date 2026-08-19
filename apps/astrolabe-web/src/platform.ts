export const platformProfiles = ["browser", "macos", "windows", "linux", "kiosk"] as const;

export type PlatformProfile = (typeof platformProfiles)[number];

declare global {
  interface Window {
    /** Set by the native launcher before the document loads. */
    __ASTROLABE_PLATFORM__?: PlatformProfile;
  }
}

function isPlatformProfile(value: string | null | undefined): value is PlatformProfile {
  return value !== null && value !== undefined && platformProfiles.includes(value as PlatformProfile);
}

/**
 * A browser does not get to claim a native identity from its user agent.
 * Desktop shells provide a trusted value; the query parameter is deliberately
 * development-only preview plumbing.
 */
export function resolvePlatform(location = window.location): PlatformProfile {
  if (isPlatformProfile(window.__ASTROLABE_PLATFORM__)) return window.__ASTROLABE_PLATFORM__;

  const requested = new URLSearchParams(location.search).get("platform");
  if (import.meta.env.DEV && isPlatformProfile(requested)) return requested;

  return "browser";
}

export function shortcutModifier(profile: PlatformProfile): string {
  return profile === "macos" ? "⌘" : "Ctrl";
}

export function shortcut(profile: PlatformProfile, key: string): string {
  return `${shortcutModifier(profile)}${key}`;
}
