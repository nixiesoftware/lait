/**
 * The World-settings window — deliberately separate from the client.
 *
 * It receives a read-only snapshot in its URL and never watches the core.
 * That keeps Astrolabe single-owner while letting settings behave like
 * desktop settings: independently movable, focusable and closed.
 */
import type { WorldSettingsSnapshot } from "./client";

export function WorldSettingsSurface({ snapshot }: { snapshot: WorldSettingsSnapshot }) {
  return <section className="settings-page" aria-label={`${snapshot.name} settings`}>
    <h1>{snapshot.name} settings</h1>
    <p className="settings-prose">Runtime and location details reported by this World.</p>
    <SettingsSection title="APPLICATION">
      <Setting label="IMPLEMENTATION VERSION"
        value={snapshot.version === null ? "Not reported" : `v${snapshot.version}`} />
    </SettingsSection>
    <SettingsSection title="LOCATIONS">
      <Setting label="WORLD MOUNT" value={snapshot.worldMount} mono />
      <Setting label="ENTRY PATH" value={snapshot.entryPath ?? "Not declared"} mono />
    </SettingsSection>
    <SettingsSection title="ACTIVE INSTANCE">
      <Setting label="ORIGIN" value={snapshot.activeOrigin ?? "Not reported"} mono />
    </SettingsSection>
  </section>;
}

function SettingsSection({ title, children }: { title: string; children: React.ReactNode }) {
  return <section className="settings-card">
    <span className="fact-label">{title}</span>
    {children}
  </section>;
}

function Setting({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return <div className="setting-row">
    <span className="fact-label">{label}</span>
    {mono ? <code>{value}</code> : <span>{value}</span>}
  </div>;
}
