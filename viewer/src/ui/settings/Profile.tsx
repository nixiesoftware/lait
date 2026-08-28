import { useCallback, useEffect, useState } from "react";
import { Bot, Check, Copy, Laptop, ShieldCheck } from "lucide-react";
import { IconButton } from "@astryxdesign/core";

import { spaceRpc } from "../../api";
import type { WhoamiInfo } from "../../types";
import { LoadingState } from "../AppState";
import { Badge } from "../primitives";
import {
  SettingsField,
  SettingsPageHeader,
  SettingsSection,
  SettingsSurface,
} from "../settingsLayout";

/**
 * Profile — who you are in this space, as the engine sees it.
 *
 * Linear's Profile is a form: name, title, username. Ours is mostly a reading,
 * and that is the honest shape. Identity here is a key, not a string: the
 * actor and device ids are derived and cannot be edited, the role comes from
 * the signed ACL graph, and the name other members see is *their* Card for you
 * in *their* address book — there is no workspace-wide display name to type.
 * So this page tells you what you can copy, what standing you hold, and what
 * would explain a refusal, rather than offering fields that would save nowhere.
 */
export function ProfilePanel({
  spaceId,
  spaceName,
  revision,
  onError,
}: {
  spaceId: string;
  spaceName: string;
  revision: number;
  onError: (message: string) => void;
}) {
  const [who, setWho] = useState<WhoamiInfo | null>(null);

  const load = useCallback(async () => {
    try {
      const reply = await spaceRpc(spaceId, { cmd: "whoami" });
      if (reply.kind === "whoami") setWho(reply);
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  }, [spaceId, onError]);

  useEffect(() => {
    void load();
  }, [load, revision]);

  if (!who) {
    return (
      <>
        <SettingsPageHeader title="Profile" />
        <LoadingState
          title="Reading your standing"
          body="Asking the daemon who you are in this space."
        />
      </>
    );
  }

  const role = who.role === "none" ? "Not a member" : who.role;

  return (
    <>
      <SettingsPageHeader
        title="Profile"
        description={`You, in ${spaceName || "this space"}. Identity is derived from keys, so it is read here and copied, never typed.`}
      />

      <SettingsSection title="Identity">
        <SettingsSurface>
          <SettingsField
            label="Name"
            hint="What this device calls you. Other members see the name they gave you in their own address book."
          >
            <div className="flex justify-end text-sm">
              {who.name?.trim() ? (
                <span className="font-medium">{who.name}</span>
              ) : (
                <span className="text-mute italic">unnamed</span>
              )}
            </div>
          </SettingsField>
          <SettingsField label="Actor" hint="Your identity across every device that signs as you">
            <CopyableId value={who.actor ?? null} label="actor id" />
          </SettingsField>
          <SettingsField label="This device" hint="The key this machine signs with">
            <CopyableId
              value={who.device}
              label="device id"
              icon={<Laptop className="text-mute size-icon-sm shrink-0" />}
            />
          </SettingsField>
        </SettingsSurface>
      </SettingsSection>

      <SettingsSection
        title="Standing"
        hint="What the signed access graph lets you do here. A refused action names the capability it wanted; this is where to look for it."
      >
        <SettingsSurface>
          <SettingsField label="Role" hint="From the signed ACL graph">
            <div className="flex items-center justify-end gap-2">
              <Badge tone={who.role === "admin" ? "accent" : "neutral"} className="capitalize">
                {who.role === "admin" && <ShieldCheck className="size-icon-xs" />}
                {role}
              </Badge>
              {who.policy_admin && <Badge tone="accent">policy admin</Badge>}
              <span className="text-mute text-xs">{who.can_write ? "can write" : "read only"}</span>
            </div>
          </SettingsField>
          {who.sponsor && (
            <SettingsField label="Sponsor" hint="Your standing here ends when theirs does">
              <div className="flex items-center justify-end gap-2 text-sm">
                <Bot className="text-mute size-icon-sm" />
                <code className="text-dim truncate font-mono text-xs">{who.sponsor}</code>
              </div>
            </SettingsField>
          )}
          <SettingsField
            label="Capabilities"
            hint={
              who.capabilities && who.capabilities.length > 0
                ? `${who.capabilities.length} granted`
                : "Only what the role carries"
            }
            align="start"
          >
            {who.capabilities && who.capabilities.length > 0 ? (
              <ul className="flex flex-wrap justify-end gap-1">
                {who.capabilities.map((capability) => (
                  <li
                    key={capability}
                    className="border-line-strong text-dim rounded-full border px-2 py-px font-mono text-2xs"
                  >
                    {capability}
                  </li>
                ))}
              </ul>
            ) : (
              <p className="text-mute text-right text-sm">None beyond the role.</p>
            )}
          </SettingsField>
          {who.partial_view && (
            <SettingsField
              label="View completeness"
              hint="This device has not converged with every peer. Writes made against a partial view are refused until it has."
            >
              <div className="flex justify-end">
                <Badge tone="danger">partial view</Badge>
              </div>
            </SettingsField>
          )}
        </SettingsSurface>
      </SettingsSection>
    </>
  );
}

/** An identifier in a code box with a copy button beside it, or a quiet dash
 *  when there is nothing to copy yet. */
function CopyableId({
  value,
  label,
  icon,
}: {
  value: string | null;
  label: string;
  icon?: React.ReactNode;
}) {
  const [copied, setCopied] = useState(false);
  if (!value) return <p className="text-mute text-right text-sm">—</p>;
  return (
    <div className="flex items-center justify-end gap-1.5">
      <div className="border-line bg-hover text-dim flex min-w-0 items-center gap-2 rounded-control border px-2 py-1.5 font-mono text-xs">
        {icon}
        <span className="min-w-0 truncate" title={value}>
          {value}
        </span>
      </div>
      <IconButton
        label={copied ? "Copied" : `Copy ${label}`}
        tooltip={copied ? "Copied" : `Copy ${label}`}
        variant="ghost"
        size="sm"
        icon={copied ? <Check className="size-icon-sm" /> : <Copy className="size-icon-sm" />}
        onClick={() => {
          void navigator.clipboard.writeText(value).then(() => {
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1500);
          });
        }}
      />
    </div>
  );
}
