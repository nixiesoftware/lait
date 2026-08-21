import { BroadcastRow } from '@/components/broadcasts/types';

export function upsertRowAt(rows: BroadcastRow[], row: BroadcastRow, index: number): BroadcastRow[] {
  const next = [...rows];
  next.splice(index, 0, row);
  return next;
}

export function removeRowById(rows: BroadcastRow[], id: string): BroadcastRow[] {
  return rows.filter(r => r.item.id !== id);
}
