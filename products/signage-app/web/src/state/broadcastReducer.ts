import { BroadcastRow } from '@/components/broadcasts/types';
import { upsertRowAt, removeRowById } from '@/utils/positions';
import { mintBodyId } from '@/utils/lait/ids';

export type BroadcastAction =
  | { type: 'INIT'; rows: BroadcastRow[] }
  | { type: 'ADD_ITEM'; row: BroadcastRow }
  | { type: 'REMOVE_ITEM'; id: string }
  | { type: 'UPDATE_DURATION'; id: string; duration_ms: number }
  | { type: 'REORDER'; rows: BroadcastRow[] }
  | { type: 'DUPLICATE_AFTER'; id: string }
  | { type: 'PASTE_AFTER'; targetId: string; row: BroadcastRow }
  | { type: 'PASTE_AT_END'; row: BroadcastRow };

export interface BroadcastState {
  rows: BroadcastRow[];
}

export function broadcastReducer(state: BroadcastState, action: BroadcastAction): BroadcastState {
  switch (action.type) {
    case 'INIT':
      return { rows: [...action.rows] };
    case 'ADD_ITEM':
      return { rows: [...state.rows, action.row] };
    case 'REMOVE_ITEM':
      return { rows: removeRowById(state.rows, action.id) };
    case 'UPDATE_DURATION':
      return {
        rows: state.rows.map(r =>
          r.item.id === action.id
            ? { ...r, item: { ...r.item, duration_ms: action.duration_ms } }
            : r,
        ),
      };
    case 'REORDER':
      return { rows: [...action.rows] };
    case 'DUPLICATE_AFTER': {
      const idx = state.rows.findIndex(r => r.item.id === action.id);
      if (idx === -1) return state;
      const source = state.rows[idx];
      const dup: BroadcastRow = {
        ...source,
        item: { ...source.item, id: mintBodyId() },
      };
      return { rows: upsertRowAt(state.rows, dup, idx + 1) };
    }
    case 'PASTE_AFTER': {
      const idx = state.rows.findIndex(r => r.item.id === action.targetId);
      if (idx === -1) return state;
      return { rows: upsertRowAt(state.rows, action.row, idx + 1) };
    }
    case 'PASTE_AT_END': {
      return { rows: [...state.rows, action.row] };
    }
    default:
      return state;
  }
}
