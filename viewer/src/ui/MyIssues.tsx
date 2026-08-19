import { useCallback, useEffect, useState } from "react";
import { CircleDot } from "lucide-react";

import { rpc } from "../api";
import type { Row, WorldPublicationId } from "../types";
import { EmptyState, LoadingState } from "./AppState";
import { interactiveRow } from "./primitives";
import { Button } from "@astryxdesign/core";

const publicationKey = (source: WorldPublicationId) => {
  const hex = (bytes: readonly number[]) => bytes
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
  return [
    hex(source.publication.manifest_root),
    hex(source.publication.implementation_digest),
    hex(source.publication.extractor_schema_digest),
    source.materialization,
  ].join(":");
};

const appendUniqueRows = (current: readonly Row[], incoming: readonly Row[]) => {
  const rows = new Map(current.map((row) => [row.doc_id, row]));
  for (const row of incoming) rows.set(row.doc_id, row);
  return [...rows.values()];
};

/** A workspace-level projection of everything assigned to the current actor.
 * Opening a row enters its owning project; the destination never silently
 * inherits whichever project happened to be open before this page. */
export function MyIssues({
  spaceId,
  revision,
  onOpen,
  onError,
}: {
  spaceId: string;
  revision: number;
  onOpen: (reff: string) => void;
  onError: (message: string) => void;
}) {
  const [rows, setRows] = useState<Row[] | null>(null);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [publication, setPublication] = useState<WorldPublicationId | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);

  const load = useCallback(
    async (
      alive: () => boolean,
      cursor: string | null = null,
      pinned: WorldPublicationId | null = null,
      append = false,
    ) => {
      if (append) setLoadingMore(true);
      try {
        const result = await rpc(spaceId, {
          cmd: "list",
          project: null,
          filter: { mine: true, all: true },
          page: { limit: 100, cursor },
        });
        if (!alive() || result.kind !== "list") return;
        if (pinned && publicationKey(result.page.publication) !== publicationKey(pinned)) {
          throw new Error("Issue continuation crossed publications; refresh your issues");
        }
        const visible = result.page.items.filter((row) => !row.tombstone);
        setRows((current) => append ? appendUniqueRows(current ?? [], visible) : visible);
        setNextCursor(result.page.next_cursor ?? null);
        setPublication(result.page.publication);
      } catch (error) {
        if (alive()) onError(error instanceof Error ? error.message : String(error));
      } finally {
        if (alive() && append) setLoadingMore(false);
      }
    },
    [spaceId, onError],
  );

  useEffect(() => {
    let alive = true;
    setRows(null);
    setNextCursor(null);
    setPublication(null);
    void load(() => alive);
    return () => {
      alive = false;
    };
  }, [load, revision]);

  if (rows === null) {
    return <LoadingState title="Loading your issues" body="Reading assignments across this workspace." />;
  }
  if (rows.length === 0) {
    return (
      <div className="flex min-h-0 flex-1 flex-col">
        <EmptyState
          icon={<CircleDot className="size-icon-lg" />}
          title="No issues assigned to you"
          body={nextCursor ? "No active assignments are present on this page." : "Issues assigned to you across every project will appear here."}
        />
        {nextCursor && publication && (
          <Button
            onClick={() => void load(() => true, nextCursor, publication, true)}
            isLoading={loadingMore}
            label="Load more assigned issues"
            variant="ghost"
            size="sm"
          />
        )}
      </div>
    );
  }

  return (
    <ul className="min-h-0 flex-1 overflow-y-auto">
      {rows.map((row) => (
        <li key={row.reff}>
          <button
            type="button"
            className={`${interactiveRow({ size: "lg" })} flex w-full items-center gap-3 px-4 py-2 text-left`}
            onClick={() => onOpen(row.key_alias ?? row.reff)}
          >
            <span className="text-mute w-20 shrink-0 truncate font-mono text-xs tabular-nums">
              {row.key_alias ?? row.reff}
            </span>
            <span className="min-w-0 flex-1 truncate text-sm font-medium">{row.title}</span>
            <span className="text-mute shrink-0 text-xs capitalize">{row.priority}</span>
          </button>
        </li>
      ))}
      {nextCursor && publication && (
        <li className="border-line/60 border-t p-2 text-center">
          <Button
            onClick={() => void load(() => true, nextCursor, publication, true)}
            isLoading={loadingMore}
            label="Load more assigned issues"
            variant="ghost"
            size="sm"
          />
        </li>
      )}
    </ul>
  );
}
