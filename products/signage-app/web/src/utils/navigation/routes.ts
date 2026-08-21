/**
 * Route generation for entity pages.
 */

import type { SignageProgram, SignageScreen } from '../lait/types';

export function getBroadcastDetailRoute(programId: string): string {
  return `/broadcast-list/broadcast/${programId}`;
}

export function getScreenDetailRoute(screenId: string): string {
  return `/screen-list/${screenId}`;
}

export function navigateToBroadcast(program: SignageProgram | string): string {
  const id = typeof program === 'string' ? program : program.id;
  return getBroadcastDetailRoute(id);
}

export function navigateToScreen(screen: SignageScreen | string): string {
  const id = typeof screen === 'string' ? screen : screen.id;
  return getScreenDetailRoute(id);
}

export const LIST_ROUTES = {
  broadcasts: '/broadcast-list',
  screens: '/screen-list',
  content: '/content-list',
} as const;
