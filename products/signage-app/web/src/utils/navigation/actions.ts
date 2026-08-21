/**
 * Create-and-navigate actions, usable outside React components.
 */

import { createProgram } from '../broadcasts/api';
import { createScreen } from '../screens/api';
import { navigateToBroadcast, navigateToScreen } from './routes';
import type { SignageProgram, SignageScreen } from '../lait/types';

interface NavigationActionOptions {
  onSuccess?: () => void;
  /** Return true to suppress default error handling. */
  onError?: (error: unknown) => boolean | void;
  router?: { push: (url: string) => void };
}

function go(route: string, router?: { push: (url: string) => void }): void {
  if (router) {
    router.push(route);
  } else if (typeof window !== 'undefined') {
    window.location.href = route;
  }
}

export async function createAndNavigateToBroadcast(
  name?: string,
  options: NavigationActionOptions = {},
): Promise<SignageProgram | null> {
  const { onSuccess, onError, router } = options;
  try {
    const program = await createProgram(name ?? 'Untitled Broadcast');
    go(navigateToBroadcast(program), router);
    onSuccess?.();
    return program;
  } catch (error) {
    if (!onError?.(error)) {
      console.error('Error creating broadcast:', error);
    }
    return null;
  }
}

export async function createAndNavigateToScreen(
  name?: string,
  options: NavigationActionOptions = {},
): Promise<SignageScreen | null> {
  const { onSuccess, onError, router } = options;
  try {
    const screen = await createScreen(name ?? 'Unnamed Screen');
    go(navigateToScreen(screen), router);
    onSuccess?.();
    return screen;
  } catch (error) {
    if (!onError?.(error)) {
      console.error('Error creating screen:', error);
    }
    return null;
  }
}
