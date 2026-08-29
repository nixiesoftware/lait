import { useEffect, useState } from "react";
import { Button, Switch } from "@astryxdesign/core";

import {
  inboxCauseLabel,
  loadInboxPreferences,
  persistInboxPreferences,
  type InboxKind,
  type InboxPreferences,
} from "../../core/inbox";
import { Combobox } from "../Picker";
import {
  SettingsField,
  SettingsPageHeader,
  SettingsSection,
  SettingsSurface,
} from "../settingsLayout";

const KINDS: readonly { id: InboxKind; hint: string }[] = [
  { id: "assigned", hint: "An issue is assigned to you, or taken away" },
  {
    id: "comment",
    hint: "Someone comments on an issue you follow, or mentions you",
  },
  { id: "status", hint: "An issue you follow changes status" },
];

const GROUPINGS = [
  {
    id: "cause",
    label: "By cause",
    hint: "Assignments, then comments, then status",
  },
  {
    id: "chronological",
    label: "Chronologically",
    hint: "Newest first, whatever it was",
  },
] as const;

/**
 * Notifications — what the Inbox shows you, decided once instead of in a
 * popover on the Inbox itself.
 *
 * The same store the Inbox's own Preferences button writes, kept per space and
 * per device: the daemon delivers the complete feed and this only chooses what
 * this device draws from it, which is why nothing here is a Space setting and
 * nothing here is gated by standing. Linear's Notifications page decides
 * delivery channels; lait has one channel — the Inbox — so this page decides
 * what reaches it.
 */
export function NotificationsPanel({ spaceId }: { spaceId: string }) {
  const [prefs, setPrefs] = useState<InboxPreferences>(() => loadInboxPreferences(spaceId));
  useEffect(() => setPrefs(loadInboxPreferences(spaceId)), [spaceId]);
  const save = (next: InboxPreferences) => {
    setPrefs(next);
    persistInboxPreferences(spaceId, next);
  };
  const grouping = GROUPINGS.find((g) => g.id === prefs.grouping) ?? GROUPINGS[0];
  const snoozedCount = Object.values(prefs.snoozed).filter((until) => until > Date.now()).length;

  return (
    <>
      <SettingsPageHeader
        title="Notifications"
        description="What reaches your Inbox in this space. Private to this device; the complete feed is still delivered."
      />

      <SettingsSection
        title="Inbox"
        hint="Each cause can be shown or hidden. A hidden cause is not deleted — turn it back on and its notifications are there."
      >
        <SettingsSurface>
          {KINDS.map((kind) => (
            <SettingsField key={kind.id} label={inboxCauseLabel(kind.id)} hint={kind.hint}>
              <div className="flex justify-end">
                <Switch
                  label={inboxCauseLabel(kind.id)}
                  isLabelHidden
                  value={prefs.kinds[kind.id]}
                  onChange={(on) => save({ ...prefs, kinds: { ...prefs.kinds, [kind.id]: on } })}
                  size="sm"
                />
              </div>
            </SettingsField>
          ))}
        </SettingsSurface>
      </SettingsSection>

      <SettingsSection title="Display">
        <SettingsSurface>
          <SettingsField label="Group notifications" hint="How the Inbox orders what it shows">
            <div className="flex justify-end">
              <Combobox
                label="Group notifications"
                value={{ id: grouping.id, label: grouping.label }}
                options={GROUPINGS.map((g) => ({
                  id: g.id,
                  label: g.label,
                  hint: g.hint,
                }))}
                onPick={(id) =>
                  save({
                    ...prefs,
                    grouping: id as InboxPreferences["grouping"],
                  })
                }
                size="md"
              />
            </div>
          </SettingsField>
          <SettingsField
            label="Snoozed"
            hint={
              snoozedCount === 0
                ? "Nothing is snoozed. A snoozed notification returns to the Inbox after an hour."
                : `${snoozedCount} ${snoozedCount === 1 ? "notification is" : "notifications are"} hidden for up to an hour.`
            }
          >
            <div className="flex justify-end">
              <Button
                label="Restore all"
                variant="secondary"
                size="sm"
                isDisabled={snoozedCount === 0}
                onClick={() => save({ ...prefs, snoozed: {} })}
              />
            </div>
          </SettingsField>
        </SettingsSurface>
      </SettingsSection>
    </>
  );
}
