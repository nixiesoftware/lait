import { Keyboard, Monitor, Moon, Sun } from "lucide-react";
import { Button, SegmentedControl, SegmentedControlItem } from "@astryxdesign/core";

import {
  HOME_VIEW_OPTIONS,
  savePreference,
  usePreferences,
  type CommentSubmit,
  type HomeView,
  type WeekStart,
} from "../../core/preferences";
import { Combobox } from "../Picker";
import {
  SettingsField,
  SettingsPageHeader,
  SettingsSection,
  SettingsSurface,
} from "../settingsLayout";

export type ThemePreference = "system" | "light" | "dark";
export type DensityPreference = "compact" | "comfortable";

const COMMENT_KEYS: readonly {
  id: CommentSubmit;
  label: string;
  hint: string;
}[] = [
  { id: "mod-enter", label: "⌘ Enter", hint: "Enter adds a line" },
  { id: "enter", label: "Enter", hint: "Shift+Enter adds a line" },
];

/**
 * Preferences — what this person wants, on this device.
 *
 * Every control here writes somewhere private: theme and density to the two
 * keys `App` has always kept, the rest to `core/preferences`. Nothing on this
 * page is a Space setting, which is why nothing on it is gated by `readOnly` —
 * a viewer with no write standing still gets to choose a dark theme.
 *
 * The rows mirror Linear's Preferences where the underlying fact exists here
 * (home view, week start, comment key, theme) and stop where it does not: no
 * "display names" row, because a name in lait is a Card in your address book
 * rather than a workspace-wide string, and no "font size", because the density
 * ladder is the one type scale the design system carries.
 */
export function PreferencesPanel({
  theme,
  onThemeChange,
  density,
  onDensityChange,
  onOpenShortcuts,
}: {
  theme: ThemePreference;
  onThemeChange: (theme: ThemePreference) => void;
  density: DensityPreference;
  onDensityChange: (density: DensityPreference) => void;
  onOpenShortcuts: () => void;
}) {
  const prefs = usePreferences();
  const home = HOME_VIEW_OPTIONS.find((o) => o.id === prefs.homeView) ?? HOME_VIEW_OPTIONS[0]!;
  const commentKey = COMMENT_KEYS.find((o) => o.id === prefs.commentSubmit) ?? COMMENT_KEYS[0]!;

  return (
    <>
      <SettingsPageHeader
        title="Preferences"
        description="How this client behaves for you. Private to this device; nothing here is shared with the space."
      />

      <SettingsSection title="General">
        <SettingsSurface>
          <SettingsField
            label="Default home view"
            hint="Which surface opens when you launch into a space"
          >
            <div className="flex justify-end">
              <Combobox
                label="Default home view"
                value={{ id: home.id, label: home.label }}
                options={HOME_VIEW_OPTIONS.map((o) => ({
                  id: o.id,
                  label: o.label,
                }))}
                onPick={(id) => savePreference("homeView", id as HomeView)}
                size="md"
              />
            </div>
          </SettingsField>
          <SettingsField label="First day of the week" hint="Used by the calendar and date pickers">
            <div className="flex justify-end">
              <SegmentedControl
                label="First day of the week"
                value={prefs.weekStart}
                onChange={(value) => savePreference("weekStart", value as WeekStart)}
                size="sm"
              >
                <SegmentedControlItem value="monday" label="Monday" />
                <SegmentedControlItem value="sunday" label="Sunday" />
              </SegmentedControl>
            </div>
          </SettingsField>
          <SettingsField label="Send comments on…" hint="Which key press submits a comment">
            <div className="flex justify-end">
              <Combobox
                label="Send comments on"
                value={{ id: commentKey.id, label: commentKey.label }}
                options={COMMENT_KEYS.map((o) => ({
                  id: o.id,
                  label: o.label,
                  hint: o.hint,
                }))}
                onPick={(id) => savePreference("commentSubmit", id as CommentSubmit)}
                size="md"
              />
            </div>
          </SettingsField>
        </SettingsSurface>
      </SettingsSection>

      <SettingsSection title="Interface and theme">
        <SettingsSurface>
          <SettingsField label="Interface theme" hint="System follows your operating system">
            <div className="flex justify-end">
              <SegmentedControl
                label="Interface theme"
                value={theme}
                onChange={(value) => onThemeChange(value as ThemePreference)}
                size="sm"
              >
                <SegmentedControlItem
                  value="system"
                  label="System"
                  icon={<Monitor className="size-icon-sm" />}
                />
                <SegmentedControlItem
                  value="light"
                  label="Light"
                  icon={<Sun className="size-icon-sm" />}
                />
                <SegmentedControlItem
                  value="dark"
                  label="Dark"
                  icon={<Moon className="size-icon-sm" />}
                />
              </SegmentedControl>
            </div>
          </SettingsField>
          <SettingsField
            label="Density"
            hint="Comfortable loosens every row and steps the type ladder up"
          >
            <div className="flex justify-end">
              <SegmentedControl
                label="Density"
                value={density}
                onChange={(value) => onDensityChange(value as DensityPreference)}
                size="sm"
              >
                <SegmentedControlItem value="compact" label="Compact" />
                <SegmentedControlItem value="comfortable" label="Comfortable" />
              </SegmentedControl>
            </div>
          </SettingsField>
        </SettingsSurface>
      </SettingsSection>

      <SettingsSection title="Keyboard">
        <SettingsSurface>
          <SettingsField
            label="Keyboard shortcuts"
            hint="Every command the client knows, with the keys that reach it"
          >
            <div className="flex justify-end">
              <Button
                label="View shortcuts"
                variant="secondary"
                size="sm"
                icon={<Keyboard className="size-icon-sm" />}
                onClick={onOpenShortcuts}
              />
            </div>
          </SettingsField>
        </SettingsSurface>
      </SettingsSection>
    </>
  );
}
