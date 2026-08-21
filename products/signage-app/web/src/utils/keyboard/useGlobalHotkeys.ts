import { useEffect } from 'react';

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName.toLowerCase();
  if (tag === 'input' || tag === 'textarea' || target.isContentEditable) return true;
  // Also treat select and buttons with data-hotkeys-exempt
  if (tag === 'select') return true;
  return !!target.closest('[data-hotkeys-exempt]');

}

export type HotkeyHandler = (e: KeyboardEvent) => void;

export function useGlobalHotkeys(handler: HotkeyHandler, deps: unknown[] = []) {
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (isEditableTarget(e.target)) return;
      handler(e);
    };
    window.addEventListener('keydown', onKeyDown);
    return () => {
      window.removeEventListener('keydown', onKeyDown);
    };
  }, deps);
}

export const Hotkey = {
  isCopy(e: KeyboardEvent) {
    return (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'c';
  },
  isPaste(e: KeyboardEvent) {
    return (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'v';
  },
  isDelete(e: KeyboardEvent) {
    return e.key === 'Delete' || e.key === 'Backspace';
  },
  isSpace(e: KeyboardEvent) {
    return e.code === 'Space' || e.key === ' ';
  },
  isArrowLeft(e: KeyboardEvent) {
    return e.key === 'ArrowLeft';
  },
  isArrowRight(e: KeyboardEvent) {
    return e.key === 'ArrowRight';
  }
};
