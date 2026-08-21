import { BroadcastRow } from '@/components/broadcasts/types';

const draftKey = (broadcastId: string) => `broadcast-draft-${broadcastId}`;
const selectionKey = (broadcastId: string) => `broadcast-selection-${broadcastId}`;

export function loadDraftRows(broadcastId: string): BroadcastRow[] | null {
  if (typeof window === 'undefined') return null;
  const raw = localStorage.getItem(draftKey(broadcastId));
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as BroadcastRow[];
    if (Array.isArray(parsed)) return parsed;
    return null;
  } catch {
    return null;
  }
}

export function saveDraftRows(broadcastId: string, rows: BroadcastRow[]): void {
  if (typeof window === 'undefined') return;
  localStorage.setItem(draftKey(broadcastId), JSON.stringify(rows));
}

export function clearDraftRows(broadcastId: string): void {
  if (typeof window === 'undefined') return;
  localStorage.removeItem(draftKey(broadcastId));
}

export function loadSelection(broadcastId: string): string | null {
  if (typeof window === 'undefined') return null;
  return localStorage.getItem(selectionKey(broadcastId));
}

export function saveSelection(broadcastId: string, itemId: string): void {
  if (typeof window === 'undefined') return;
  localStorage.setItem(selectionKey(broadcastId), itemId);
}

export function clearSelection(broadcastId: string): void {
  if (typeof window === 'undefined') return;
  localStorage.removeItem(selectionKey(broadcastId));
}
