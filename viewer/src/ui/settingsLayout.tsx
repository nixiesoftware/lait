import { cn } from "./primitives";

/** The title row shared by every settings sub-page. Collection actions live at
 * the far edge; form pages simply omit them. */
export function SettingsPageHeader({
  title,
  description,
  actions,
  className,
}: {
  title: string;
  description?: string;
  actions?: React.ReactNode;
  className?: string;
}) {
  return (
    <header className={cn("mb-7 flex items-start gap-4", className)}>
      <div className="min-w-0 flex-1">
        <h1 className="text-xl font-semibold tracking-tight">{title}</h1>
        {description && <p className="text-mute mt-1 max-w-2xl text-sm">{description}</p>}
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
    </header>
  );
}

/** A named block within a settings page. */
export function SettingsSection({
  title,
  hint,
  actions,
  children,
  className,
}: {
  title: string;
  hint?: string;
  actions?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <section className={cn("mb-9", className)}>
      <div className="flex items-start gap-4">
        <div className="min-w-0 flex-1">
          <h2 className="text-base font-semibold">{title}</h2>
          {hint && <p className="text-mute mt-0.5 text-sm">{hint}</p>}
        </div>
        {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
      </div>
      <div className="mt-3">{children}</div>
    </section>
  );
}

/** A quiet grouped surface for related settings. Direct children become rows,
 * so every settings page shares the same outline, fill, and separators. */
export function SettingsSurface({
  children,
  className,
}: {
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "settings-surface border-line divide-line overflow-hidden rounded-surface border divide-y",
        className,
      )}
    >
      {children}
    </div>
  );
}

/** Linear-style settings row: meaning on the left, control or value on the
 * right. Inputs keep their own visually-hidden accessible labels. */
export function SettingsField({
  label,
  hint,
  children,
  align = "center",
  className,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
  align?: "center" | "start";
  className?: string;
}) {
  return (
    <div
      className={cn(
        "grid grid-cols-[minmax(0,1fr)_minmax(16rem,24rem)] gap-6 px-4 py-3 max-[640px]:grid-cols-1 max-[640px]:gap-3",
        align === "start" ? "items-start" : "items-center",
        className,
      )}
    >
      <div className="min-w-0">
        <div className="text-sm font-semibold">{label}</div>
        {hint && <p className="text-mute mt-0.5 text-xs leading-relaxed">{hint}</p>}
      </div>
      <div className="min-w-0">{children}</div>
    </div>
  );
}
