/**
 * Integration configs. The World stores one `SignageConfig` per kind and
 * refuses a second; settings are an untyped string map on purpose — what a
 * kind's settings *mean* is this application's knowledge, and it now lives in
 * `program-editor/kinds`, where the same declaration renders the panel, the
 * preview and the summary. This module is the transport and nothing else.
 */

import { rpc } from '../api/client';
import { mintBodyId } from '../lait/ids';
import type {
  ConfigReply,
  ConfigSavedReply,
  ConfigsReply,
  SignageConfig,
} from '../lait/types';

export async function fetchConfigs(): Promise<SignageConfig[]> {
  const reply = await rpc<ConfigsReply>({ cmd: 'config_list' });
  return reply.configs;
}

export async function fetchConfigByKind(kind: string): Promise<SignageConfig | null> {
  const configs = await fetchConfigs();
  return configs.find((config) => config.kind === kind) ?? null;
}

export async function fetchConfig(id: string): Promise<SignageConfig | null> {
  const reply = await rpc<ConfigReply>({ cmd: 'config_get', config: id });
  return reply.config;
}

export async function putConfig(
  kind: string,
  name: string,
  settings: Record<string, string>,
): Promise<string> {
  const existing = await fetchConfigByKind(kind);
  const config: SignageConfig = {
    id: existing?.id ?? mintBodyId(),
    kind,
    name,
    settings,
  };
  const reply = await rpc<ConfigSavedReply>({ cmd: 'config_put', config });
  return reply.config;
}

export async function deleteConfig(id: string): Promise<void> {
  await rpc({ cmd: 'config_delete', config: id }, { confirm: true });
}
