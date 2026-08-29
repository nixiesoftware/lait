/**
 * The design system: Fluent UI v9, the Windows 11 language, applied once at
 * the root of every window. Every surface reaches components and tokens from
 * here rather than from a stylesheet of its own, which is the whole point —
 * one system, reproduced the same way by whoever builds the next surface.
 */
import { FluentProvider, webDarkTheme, webLightTheme, type Theme } from "@fluentui/react-components";
import type { ReactNode } from "react";

/**
 * Astrolabe's two themes. Fluent's own scales, with the brand indigo the
 * client already wears; nothing else is overridden, so what Fluent draws is
 * what the OS draws.
 */
const dark: Theme = { ...webDarkTheme, colorBrandBackground: "#5b8def", colorBrandBackgroundHover: "#6f9cf2", colorBrandBackgroundPressed: "#4a7ade" };
const light: Theme = { ...webLightTheme, colorBrandBackground: "#3d6fd6", colorBrandBackgroundHover: "#5183e6", colorBrandBackgroundPressed: "#2f5bb5" };

export function AstrolabeTheme({ dark: isDark, children }: { dark: boolean; children: ReactNode }) {
  // The provider paints the window's ground: surfaces that still carry
  // their own background sit on it until they are moved over.
  return <FluentProvider theme={isDark ? dark : light} style={{ height: "100%", minHeight: 0 }}>
    {children}
  </FluentProvider>;
}
