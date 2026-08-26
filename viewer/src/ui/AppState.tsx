import { useState } from "react";
import {
  AlertTriangle,
  CheckCircle2,
  CloudOff,
  Copy,
  Database,
  HardDrive,
  LoaderCircle,
  RefreshCw,
  SearchX,
  ShieldCheck,
  Users,
  X,
} from "lucide-react";

import { errorKindOf } from "../api";
import type { SpaceRow, StatusInfo, WhoamiInfo } from "../types";
import { Button, Popover, Skeleton } from "@astryxdesign/core";
import { cn } from "./primitives";

export type ApplicationStateKind =
  | "loading"
  | "empty"
  | "filtered-empty"
  | "unavailable"
  | "error"
  | "retry"
  | "progress"
  | "success";

export function ApplicationState({
  kind,
  icon,
  title,
  body,
  action,
  className,
}: {
  kind: ApplicationStateKind;
  icon?: React.ReactNode;
  title: string;
  body?: React.ReactNode;
  action?: React.ReactNode;
  className?: string;
}) {
  // A filter matching nothing is not a first run, and should not cost what one
  // costs. Somebody who has filtered a list knows what the list holds and what
  // an issue is; the only thing they do not know is that this filter matched
  // none of them, and the only thing they want is out. So it is a line and an
  // escape — no heading weight, no prose restating the button underneath it.
  //
  // The states that teach keep their full shape. That difference is the whole
  // point: how much a surface spends should track how much the person still has
  // to be told.
  const quiet = kind === "filtered-empty";
  return (
    <div
      className={cn("flex flex-1 items-center justify-center", quiet ? "p-6" : "p-8", className)}
      data-application-state={kind}
      role={kind === "error" || kind === "retry" ? "alert" : "status"}
      aria-live={kind === "loading" || kind === "progress" ? "polite" : undefined}
      aria-busy={kind === "loading" || kind === "progress" ? true : undefined}
    >
      <div className="flex max-w-sm flex-col items-center text-center">
        <span
          className={cn(
            "text-mute",
            quiet ? "mb-2" : "mb-3",
            (kind === "error" || kind === "retry") && "text-danger",
          )}
        >
          {icon ?? <StateIcon kind={kind} />}
        </span>
        {quiet ? (
          <h2 className="text-dim text-sm">{title}</h2>
        ) : (
          <h2 className="text-base font-semibold">{title}</h2>
        )}
        {body && !quiet && <p className="text-dim mt-1 text-sm leading-5">{body}</p>}
        {action && <div className={quiet ? "mt-3" : "mt-4"}>{action}</div>}
      </div>
    </div>
  );
}

export function EmptyState(props: Omit<React.ComponentProps<typeof ApplicationState>, "kind"> & { kind?: Extract<ApplicationStateKind, "empty" | "filtered-empty" | "unavailable"> }) {
  const { kind = "empty", ...rest } = props;
  return <ApplicationState kind={kind} {...rest} />;
}

/**
 * The shape of the list that is coming, while it comes.
 *
 * A centred spinner over an empty pane says "something is happening" and
 * nothing else: the pane is blank, the chrome is absent, and when the rows
 * arrive everything jumps into place at once. Rows are a fixed `h-ctl-xl` — a
 * rung on the control ladder, scaled by `--scale` — so standing in for them at
 * the same rung means the arrival costs no movement.
 *
 * The `Skeleton` is the design system's own, and so is `--color-skeleton`: both
 * were already here, and neither had a caller.
 *
 * Marked `aria-hidden` and wrapped in a live region that says the honest thing
 * once. A screen reader has no use for eight grey bars, and eight of them
 * announced individually is worse than silence.
 */
export function SkeletonRows({ rows = 8, label = "Loading" }: { rows?: number; label?: string }) {
  return (
    <div className="flex flex-1 flex-col" data-application-state="loading" aria-busy>
      <span className="sr-only" role="status" aria-live="polite">
        {label}
      </span>
      <div aria-hidden className="flex flex-col">
        {Array.from({ length: rows }, (_, row) => (
          <div key={row} className="border-line h-ctl-xl flex items-center gap-3 border-b px-4">
            <Skeleton width={16} height={16} radius={4} />
            <Skeleton width={64} height={10} radius={4} />
            {/* Uneven, because a list is: a column of identical bars reads as a
                table of one repeated value rather than as titles arriving. */}
            <Skeleton width={`${34 + ((row * 13) % 38)}%`} height={10} radius={4} />
            <span className="flex-1" />
          </div>
        ))}
      </div>
    </div>
  );
}

export function LoadingState(props: Omit<React.ComponentProps<typeof ApplicationState>, "kind">) {
  return <ApplicationState kind="loading" {...props} />;
}

export function ProgressState(props: Omit<React.ComponentProps<typeof ApplicationState>, "kind">) {
  return <ApplicationState kind="progress" {...props} />;
}

function StateIcon({ kind }: { kind: ApplicationStateKind }) {
  if (kind === "loading" || kind === "progress") return <LoaderCircle className="size-icon-lg animate-spin" />;
  if (kind === "filtered-empty") return <SearchX className="size-icon-md" />;
  if (kind === "error" || kind === "retry" || kind === "unavailable") return <AlertTriangle className="size-icon-lg" />;
  if (kind === "success") return <CheckCircle2 className="text-ok size-icon-lg" />;
  return <Database className="size-icon-lg" />;
}

export function InlineError({
  title,
  message,
  retryLabel = "Retry",
  onRetry,
  onCopy,
  onDismiss,
  failureKind,
}: {
  title?: string;
  message: string;
  retryLabel?: string;
  onRetry?: () => void;
  onCopy?: () => void;
  onDismiss?: () => void;
  failureKind?: FailureKind;
}) {
  return (
    <div className="border-danger/25 bg-danger/5 text-danger flex items-center gap-2 border-b px-3 py-2 text-sm" role="alert" data-failure-kind={failureKind}>
      <AlertTriangle className="size-icon-sm shrink-0" />
      <span className="min-w-0 flex-1">
        {title && <strong className="mr-1">{title}.</strong>}
        {message}
      </span>
      {onRetry && (
        <Button
          onClick={onRetry}
          icon={<RefreshCw className="size-icon-xs" />}
          label={retryLabel}
          variant="danger"
          size="sm"
        />
      )}
      {onCopy && (
        <Button
          onClick={onCopy}
          icon={<Copy className="size-icon-xs" />}
          label="Copy details"
          variant="danger"
          size="sm"
        />
      )}
      {onDismiss && (
        <Button
          onClick={onDismiss}
          className="text-danger"
          label="Dismiss error"
          variant="ghost"
          size="sm">
          <X className="size-icon-xs" />
        </Button>
      )}
    </div>
  );
}

/**
 * The standing gate's face: this node's actor can't write here, so the write
 * affordances are off — said up front, instead of every button being
 * discovered dead at RPC time. Copy distinguishes "not admitted yet" from
 * "admitted view-only", because the remedies differ (wait for sync vs. ask an
 * admin). Rendered only when the gate is standing (never for agent-custody
 * rows, whose read-only face is about whose key signs, not grants).
 */
export function StandingNotice({
  standing,
  onRefresh,
}: {
  standing: WhoamiInfo;
  onRefresh?: () => void;
}) {
  const pending = !standing.member;
  return (
    <div
      className="border-warn/25 bg-warn/5 text-warn flex items-center gap-2 border-b px-3 py-2 text-sm"
      role="status"
      data-standing-gate={pending ? "pending" : "view-only"}
    >
      <ShieldCheck className="size-icon-sm shrink-0" />
      <span className="min-w-0 flex-1">
        {pending ? (
          <>
            <strong className="mr-1">Waiting for admission.</strong>
            Your membership hasn’t been sealed on this device yet — it completes
            once a peer is online. Reading is fine; writing unlocks itself.
          </>
        ) : (
          <>
            <strong className="mr-1">View-only.</strong>
            Your role here doesn’t hold write access. If an admin just granted
            it, it syncs in on its own; otherwise ask an admin.
          </>
        )}
      </span>
      {onRefresh && (
        <Button
          onClick={onRefresh}
          className="text-warn"
          icon={<RefreshCw className="size-icon-xs" />}
          label="Re-check"
          variant="ghost"
          size="sm"
        />
      )}
    </div>
  );
}

export type FailureKind =
  | "offline"
  | "incompatible"
  | "authorization"
  | "read-only"
  | "invalid-reference"
  | "stale"
  | "ambiguity"
  | "conflict"
  | "provisional"
  | "corrupt"
  | "rejected"
  | "pending-sync"
  | "authority-unavailable"
  | "unknown";

export function classifyFailure(message: string): FailureKind {
  // The engine tags every World error with a typed `error_kind`; when this
  // message arrived through the API layer, that tag wins over any regex —
  // a denial stayed "unknown" for as long as its wording drifted from the
  // patterns below.
  switch (errorKindOf(message)) {
    case "denied": return "authorization";
    case "not_found": return "invalid-reference";
    case "retry": return "conflict";
  }
  if (/read.?only/i.test(message)) return "read-only";
  if (/could not evaluate authority state|ledger problem/i.test(message)) return "authority-unavailable";
  if (/permission|unauthori|forbidden|standing|not admitted|admit or re-admit|grant/i.test(message)) return "authorization";
  if (/version|schema|implementation mismatch|incompatible|upgrade required/i.test(message)) return "incompatible";
  if (/connect|daemon|network|fetch|offline/i.test(message)) return "offline";
  if (/not found|unknown (issue|project|reference)|invalid ref/i.test(message)) return "invalid-reference";
  if (/stale|expected (revision|head)|head changed/i.test(message)) return "stale";
  if (/ambiguous|multiple matches/i.test(message)) return "ambiguity";
  if (/conflict|collision|concurrent/i.test(message)) return "conflict";
  if (/provisional|still arriving/i.test(message)) return "provisional";
  if (/corrupt|undecodable|malformed/i.test(message)) return "corrupt";
  if (/pending|queued|synchroniz/i.test(message)) return "pending-sync";
  if (/reject|refused|validation|invalid/i.test(message)) return "rejected";
  return "unknown";
}

export function recoveryForError(message: string): {
  title: string;
  retryLabel: string;
} {
  switch (classifyFailure(message)) {
    case "offline": return { title: "Local service unavailable", retryLabel: "Reconnect" };
    case "incompatible": return { title: "Viewer update required", retryLabel: "Refresh" };
    case "authorization": return { title: "Change not allowed", retryLabel: "Refresh" };
    case "read-only": return { title: "Read-only space", retryLabel: "Refresh" };
    case "invalid-reference": return { title: "Reference unavailable", retryLabel: "Refresh" };
    case "stale": return { title: "Data changed elsewhere", retryLabel: "Reload" };
    case "ambiguity": return { title: "Reference is ambiguous", retryLabel: "Refresh" };
    case "conflict": return { title: "Concurrent change detected", retryLabel: "Reload" };
    case "provisional": return { title: "Data is still arriving", retryLabel: "Refresh" };
    case "corrupt": return { title: "Stored data needs attention", retryLabel: "Refresh" };
    case "rejected": return { title: "Change rejected", retryLabel: "Retry" };
    case "pending-sync": return { title: "Change is pending", retryLabel: "Refresh" };
    case "authority-unavailable": return { title: "Authority state unavailable", retryLabel: "Retry" };
    default: return { title: "Something didn’t finish", retryLabel: "Retry" };
  }
}

/**
 * Four facts, never one "sync" lamp:
 * browser↔daemon health, local projection readiness, peer reachability, and
 * recovery custody. The current contract does not prove per-change convergence,
 * so this component deliberately makes no "synced everywhere" claim.
 */
export function TrustPopover({
  liveness,
  status,
  space,
  localReady,
  latestChange,
}: {
  liveness: "connecting" | "live" | "retrying";
  status: StatusInfo | null;
  space: SpaceRow | null;
  localReady: boolean;
  latestChange?: string;
}) {
  const [diagnosticsCopied, setDiagnosticsCopied] = useState(false);
  const peers = status?.online_peers ?? 0;
  const recoveryFailures = status?.degraded_recovery ?? [];
  const degraded = recoveryFailures.length > 0;
  const healthy = liveness === "live" && localReady && !degraded && status?.membership !== "pending";
  const agent = space?.identity.kind === "agent" ? space.identity.name : null;

  return (
    <Popover
      alignment="end"
      // The width belongs to the Popover, not to the content: Astryx sizes the
      // panel before this renders and caps it at `max-width: 100%`, so a width
      // stated on the child overflows the shell instead of setting it.
      width={320}
      content={
        <div className="p-3">
          <div className="mb-3 flex items-center gap-2">
            <ShieldCheck className="text-accent size-icon-md" />
            <div>
              <p className="font-semibold">Local trust and availability</p>
              <p className="text-mute text-xs">Facts from this device, not cloud-style guesses.</p>
            </div>
          </div>
          <dl className="flex flex-col gap-2 text-sm">
            <Fact icon={<HardDrive />} label="Local service" value={livenessLabel(liveness)} ok={liveness === "live"} />
            <Fact icon={<Database />} label="Local data" value={localReady ? "Ready" : "Loading or unavailable"} ok={localReady} />
            <Fact
              icon={peers ? <Users /> : <CloudOff />}
              label="Peer reachability"
              value={peers ? `${peers} connected` : "No peers connected"}
              ok={peers > 0}
              neutral={peers === 0}
            />
            <Fact
              icon={<Users />}
              label="Last peer contact"
              value="Not reported"
              ok={false}
              neutral
            />
            <Fact
              icon={<Database />}
              label="Latest change"
              value={latestChange || "No change pending"}
              ok={!latestChange || latestChange.includes("saved on this device")}
              neutral={!latestChange || latestChange.startsWith("Saving")}
            />
            <Fact
              icon={<ShieldCheck />}
              label="Recovery custody"
              value={degraded ? "Needs attention" : recoveryLabel(status)}
              ok={!degraded}
            />
            <Fact
              icon={<ShieldCheck />}
              label="Peer convergence"
              value="Not reported"
              ok={false}
              neutral
            />
          </dl>
          {degraded && (
            <section className="border-warn/30 bg-warn/5 mt-3 rounded-control border p-2.5" aria-label="Recovery required">
              <div className="flex items-start gap-2">
                <AlertTriangle className="text-warn mt-0.5 size-icon-sm shrink-0" />
                <div className="min-w-0 flex-1">
                  <p className="text-sm font-medium">Recovery material needs attention</p>
                  <p className="text-dim mt-0.5 text-xs leading-4">
                    Local issue data remains readable. Do not remove or replace recovery files until you have inspected the diagnosis and verified a backup.
                  </p>
                </div>
              </div>
              <ul className="border-line mt-2 space-y-1 border-t pt-2 text-xs">
                {recoveryFailures.map((failure) => (
                  <li key={failure.transcript} className="grid grid-cols-[1fr_auto] gap-2">
                    <span className="min-w-0 truncate font-mono" title={failure.transcript}>
                      {failure.transcript}
                    </span>
                    <span className="text-warn">
                      {recoveryCauseLabel(failure.reason)}
                      {failure.is_current_authority ? " · current authority" : ""}
                    </span>
                    <span className="text-dim col-span-2 break-words">
                      {failure.reason.kind === "io" ? failure.reason.detail : failure.reason.kind}
                    </span>
                  </li>
                ))}
              </ul>
              <div className="mt-2 flex items-center gap-2">
                <Button
                  onClick={() => {
                    void navigator.clipboard.writeText(recoveryDiagnostics(status));
                    setDiagnosticsCopied(true);
                    window.setTimeout(() => setDiagnosticsCopied(false), 1600);
                  }}
                  icon={<Copy className="size-icon-xs" />}
                  label={diagnosticsCopied ? "Copied" : "Copy diagnosis"}
                  variant="ghost"
                  size="sm"
                />
                <span className="text-mute text-xs">
                  Repair from Settings → Devices &amp; recovery.
                </span>
              </div>
            </section>
          )}
          {peers === 0 && localReady && (
            <p className="bg-bg border-line text-dim mt-3 rounded-surface border p-2 text-xs">
              Ready locally. Changes will share when a peer connects.
            </p>
          )}
          <p className="text-mute mt-3 text-xs leading-4">
            “Saved on this device” means the local daemon accepted the change. Peer count shows reachability only; this build does not report per-change peer acknowledgement or convergence.
          </p>
          <div className="border-line text-mute mt-3 border-t pt-2 text-xs">
            Acting as {agent ? <strong className="text-fg">agent {agent}</strong> : <strong className="text-fg">your local actor</strong>}
            {status?.membership ? ` · ${status.membership}` : ""}
          </div>
        </div>
      }
    >
      <button type="button" className={cn(
          "hover:bg-hover flex h-ctl-sm min-w-6 shrink-0 items-center gap-1.5 whitespace-nowrap rounded-control px-2 text-xs",
          healthy ? "text-dim" : "text-warn",
        )} aria-label="Local and peer status">
        <span className={cn("size-mark-xs rounded-full", healthy ? "bg-ok" : "bg-warn animate-pulse")} />
        <span className="max-[1200px]:hidden">
          {trustSummary(liveness, localReady, peers, degraded)}
        </span>
      </button>
    </Popover>
  );
}

export function recoveryDiagnostics(status: StatusInfo | null): string {
  if (!status) return "Lait recovery diagnosis\nStatus unavailable";
  const failures = status.degraded_recovery ?? [];
  return [
    "Lait recovery diagnosis",
    `Space: ${status.name} (${status.space ?? "unavailable"})`,
    `Membership: ${status.membership}`,
    `Recovery: ${recoveryLabel(status)}`,
    `Generation: ${status.recovery?.generation ?? "not reported"}`,
    ...failures.flatMap((failure) => [
      `Transcript: ${failure.transcript}`,
      `Failure: ${failure.reason.kind}${failure.reason.kind === "io" ? `: ${failure.reason.detail}` : ""}`,
      `Current authority: ${failure.is_current_authority === true ? "yes" : "no"}`,
    ]),
  ].join("\n");
}

export function trustSummary(
  liveness: "connecting" | "live" | "retrying",
  localReady: boolean,
  peers: number,
  degraded: boolean,
): string {
  if (degraded) return "Recovery needs attention";
  if (liveness !== "live") return localReady ? "Offline · local data safe" : livenessLabel(liveness);
  if (!localReady) return "Loading local data";
  return peers > 0 ? `${peers} ${peers === 1 ? "peer" : "peers"}` : "Ready locally";
}

function livenessLabel(liveness: "connecting" | "live" | "retrying"): string {
  return { connecting: "Connecting", live: "Connected", retrying: "Reconnecting" }[liveness];
}

function Fact({
  icon,
  label,
  value,
  ok,
  neutral = false,
}: {
  icon: React.ReactElement;
  label: string;
  value: string;
  ok: boolean;
  neutral?: boolean;
}) {
  return (
    <div className="grid grid-cols-[16px_1fr_auto] items-center gap-2">
      <span className={cn("[&>svg]:size-icon-sm", ok || neutral ? "text-mute" : "text-warn")}>{icon}</span>
      <dt className="text-dim">{label}</dt>
      <dd className="flex items-center gap-1.5 text-right">
        {ok ? (
          <CheckCircle2 className="text-ok size-icon-xs" />
        ) : neutral ? null : (
          <AlertTriangle className="text-warn size-icon-xs" />
        )}
        {value}
      </dd>
    </div>
  );
}

function recoveryLabel(status: StatusInfo | null): string {
  const custody = status?.recovery?.custody.state;
  if (!custody) return "Not reported";
  return {
    not_holder: "Not a holder",
    ready: "Ready on this device",
    missing: "Share missing",
    backup_unverified: "Backup unverified",
    unreadable: "Share unreadable",
  }[custody];
}

function recoveryCauseLabel(cause: NonNullable<StatusInfo["degraded_recovery"]>[number]["reason"]): string {
  return {
    wrong_protector: "Wrong protector",
    permission_denied: "Permission denied",
    corrupt: "Corrupt",
    io: "I/O failure",
  }[cause.kind];
}
