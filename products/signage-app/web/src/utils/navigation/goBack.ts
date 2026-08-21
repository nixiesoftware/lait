// A safe goBack helper that avoids hard reloads
// and falls back to a known route when history cannot go back.
export type NavigateFn = (opts: { to: string; replace?: boolean }) => void;

export function goBack(navigate: NavigateFn, fallbackHref: string) {
  // SSR safety
  if (typeof window === 'undefined') {
    navigate({ to: fallbackHref });
    return;
  }

  const canUseLength = typeof window.history?.length === 'number' && window.history.length > 1;

  if (canUseLength) {
    window.history.back();
  } else {
    navigate({ to: fallbackHref, replace: true });
  }
}
