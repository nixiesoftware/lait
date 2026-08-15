import type { Row } from "../types";
import { cmdkFilter } from "./fuzzy";

/**
 * The exact client-side search pass used by IssueSearch.
 *
 * Kept pure so the focused scan baseline can measure the same map/filter/sort
 * work the dialog performs, without mounting React or inventing a second
 * search implementation. An empty query deliberately returns the existing
 * array: opening the dialog does not copy or reorder the active rows.
 */
export function searchIssueRows(available: Row[], query: string): Row[] {
  const text = query.trim();
  if (!text) return available;
  return available
    .map((row) => ({
      row,
      score: cmdkFilter(row.key_alias ?? row.reff, text, [
        row.title,
        row.reff,
        row.project_id,
        row.status,
      ]),
    }))
    .filter(({ score }) => score > 0)
    .sort((a, b) => b.score - a.score)
    .map(({ row }) => row);
}
