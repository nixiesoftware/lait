import { useEffect, useState } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ProgramEditor } from "@/program-editor";
import { fetchProgram } from "@/utils/broadcasts/api";
import { fetchLibrary } from "@/utils/content/api";
import { current, putProgram, useFleet } from "@/utils/screens/fleet";
import { draftNameKey } from "@/utils/navigation/actions";
import { goBack } from "@/utils/navigation/goBack";
import type { SignageMedia, SignageProgram } from "@/utils/lait/types";

/**
 * The editor's page. The program and the library are what the fleet already
 * holds, so the editor opens on the same frame the row was pressed; the
 * World's own copy is read behind that and adopted if it differs.
 */
export default function BroadcastPage() {
  const params = useParams({ strict: false });
  const programId = String(params.id ?? "");
  const navigate = useNavigate();
  const fleet = useFleet();
  const known = current().programs.find((row) => row.id === programId) ?? null;
  const [program, setProgram] = useState<SignageProgram | null>(known);
  const [library, setLibrary] = useState<SignageMedia[]>(current().media);
  const [persisted, setPersisted] = useState(known != null);
  const [loading, setLoading] = useState(known == null && fleet.loading);
  const [error, setError] = useState("");

  const refreshLibrary = async (): Promise<SignageMedia[]> => {
    const catalog = await fetchLibrary();
    setLibrary(catalog);
    return catalog;
  };

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      setError("");
      try {
        const [loaded, catalog] = await Promise.all([fetchProgram(programId), fetchLibrary()]);
        if (cancelled) return;
        setLibrary(catalog);
        if (loaded) {
          setProgram((held) => held ?? loaded.program);
          setPersisted(true);
          return;
        }
        const draftName = sessionStorage.getItem(draftNameKey(programId));
        if (!draftName) {
          setProgram((held) => {
            if (!held) setError("This program is not on the World, and no draft name was kept.");
            return held;
          });
          return;
        }
        setPersisted(false);
        setProgram(
          (held) =>
            held ?? {
              id: programId,
              name: draftName,
              cycle: "loop",
              items: [],
              windows: [],
            },
        );
      } catch (err) {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : "The program could not be loaded.");
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    };
    void load();
    return () => {
      cancelled = true;
    };
  }, [programId]);

  if (!program && loading) {
    return <p className="pe-hint">Loading program…</p>;
  }
  if (error || !program) {
    return <p className="pe-error">{error || "Missing program."}</p>;
  }

  return (
    <ProgramEditor
      initial={program}
      library={library}
      persisted={persisted}
      onRefreshLibrary={refreshLibrary}
      onClose={() => goBack(navigate, "/broadcast-list")}
      onDraft={async (next) => {
        if (next.items.length === 0) {
          throw new Error("A program needs at least one item before it can be saved.");
        }
        // The draft rides on the program as it is on air; a screen keeps
        // showing the on-air fields until this is put on air.
        const held: SignageProgram = {
          ...program,
          draft: { name: next.name, cycle: next.cycle, items: next.items, windows: next.windows },
        };
        await putProgram(held);
        sessionStorage.removeItem(draftNameKey(programId));
        setProgram(held);
        setPersisted(true);
      }}
      onAir={async (next) => {
        if (next.items.length === 0) {
          throw new Error("A program needs at least one item before it can go on air.");
        }
        const aired: SignageProgram = { ...next, draft: undefined };
        await putProgram(aired);
        sessionStorage.removeItem(draftNameKey(programId));
        setProgram(aired);
        setPersisted(true);
      }}
    />
  );
}
